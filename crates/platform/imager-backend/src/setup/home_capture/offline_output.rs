//! Original-source output custody, never a completed-export receipt.
//! Native mounting is a separate root-controller seam, not app-supplied authority.
mod applications;
pub mod bootstrap;
mod export;
pub use applications::{CommittedOriginalApplicationsSelection, VerifiedOriginalApplications};
mod verified;
pub use verified::{CommittedOriginalSelection, VerifiedOriginalHome};
mod inventory_scratch;
mod native;
mod runner;
use super::{OfflineReadAdmission, offline_read::OriginalApplicationsOutput, store::Store};
use crate::media_layout::MediaPartitionBinding;
pub use export::OriginalExportOutcome;
use launcher::owned_service::{
    CaptureAdmission, OriginalOutputAdmission, OriginalOutputBinding, OriginalOutputCustody,
    OriginalOutputFiles, ServiceAuthority, ServiceCancellation,
};
pub use native::{BootMedium, NativeMediumOutput, with_boot_medium};
use std::{
    fs::File,
    io,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

/// Root-controller mount ownership seam. Not implemented by app requests or
/// MediaGrant. The implementation must retain raw disk/partition/mount lease,
/// verify owned mount namespace and absence of foreign writable aliases, and
/// create only a fresh fixed staging child through canonical private nofollow
/// provisioning. It must not release that mount before supervised child exit.
/// No caller path/UUID or deserialized token constructs this ownership.
pub(in crate::setup::home_capture) trait OwnedMediumOutput:
    ServiceAuthority
{
    fn inventory_scratch(&self) -> io::Result<(File, u64)> {
        Err(refused(
            "Original review needs admitted backed inventory storage",
        ))
    }
    /// Defaults refuse: generic output directories do not promise paired
    /// checkpoint, readonly-root projection or signed-helper custody.
    fn admit_application_output(&self, _proof: &OriginalApplicationsOutput) -> io::Result<()> {
        Err(refused(
            "Original application output custody is unavailable",
        ))
    }
    fn output_directory(&self) -> io::Result<Arc<crate::setup::migration::PinnedDirectory>> {
        Err(refused("Original verified directory custody unavailable"))
    }
    fn completed_directory(
        &self,
        _store: &Store,
        _expected: &crate::setup::portable_home_media::Expectation,
    ) -> io::Result<Arc<crate::setup::migration::PinnedDirectory>> {
        Err(refused("Original completion directory custody unavailable"))
    }
    fn completed_directory_paired(
        &self,
        store: &Store,
        expected: &super::store::OriginalOutputExpectations<'_>,
    ) -> io::Result<Arc<crate::setup::migration::PinnedDirectory>> {
        if expected.applications.is_some() {
            return Err(refused("Paired setup generation custody unavailable"));
        }
        self.completed_directory(store, expected.home)
    }
    fn committed_selection_directory(
        &self,
    ) -> io::Result<Arc<crate::setup::migration::PinnedDirectory>> {
        Err(refused("Committed selection directory custody unavailable"))
    }
    fn publish_verified_applications_receipt(
        &self,
        _expected: &crate::setup::flatpak_transfer::portable::Expectation,
        _exit: &launcher::owned_service::QuiescentExit,
        _key: &launcher::owned_service::ServiceLeaseKey,
        _work: &Path,
    ) -> io::Result<()> {
        Err(refused("Applications-only publication custody unavailable"))
    }
    fn completed_applications_directory(
        &self,
        _store: &Store,
        _expected: &crate::setup::flatpak_transfer::portable::Expectation,
    ) -> io::Result<Arc<crate::setup::migration::PinnedDirectory>> {
        Err(refused("Applications-only completion custody unavailable"))
    }
    fn files(&self) -> io::Result<OriginalOutputFiles>;
    fn mount_binding(&self) -> io::Result<OriginalOutputBinding>;
    fn publish_verified_receipt_paired(
        &self,
        expected: &super::store::OriginalOutputExpectations<'_>,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        if expected.applications.is_some() {
            return Err(refused(
                "Original application publication custody unavailable",
            ));
        }
        self.publish_verified_receipt(expected.home, exit, key, work)
    }
    fn publish_verified_receipt(
        &self,
        expected: &crate::setup::portable_home_media::Expectation,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()>;
}
pub struct OfflineOutputTransaction {
    read: Arc<OfflineReadAdmission>,
    applications: Option<OriginalApplicationsOutput>,
    store: Mutex<Store>,
    intent: File,
    intent_bytes: Vec<u8>,
    medium: Arc<dyn OwnedMediumOutput>,
    binding: OriginalOutputBinding,
    cancellation: ServiceCancellation,
    // ReadAdmission uses its existing AtomicBool cancellation API. This is
    // never a new independent decision: check() first observes parent token.
    cancelled: AtomicBool,
}
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
impl OfflineOutputTransaction {
    fn record_namespace(&self, facts: &launcher::owned_service::ServiceCustody) -> io::Result<()> {
        self.check()?;
        self.store
            .lock()
            .map_err(|_| refused("Original output Store lock failed"))?
            .record_original_output_namespace(
                &self.binding.attempt,
                &self.medium.files()?.output,
                facts,
            )?;
        self.check()
    }
    pub fn mount(
        read: Arc<OfflineReadAdmission>,
        cancellation: ServiceCancellation,
    ) -> io::Result<(Arc<Self>, Arc<NativeMediumOutput>)> {
        let (medium, store) = NativeMediumOutput::mount(read.clone(), cancellation.clone())?;
        let transaction = Self::admit(read, store, medium.clone(), cancellation)?;
        Ok((transaction, medium))
    }
    pub(in crate::setup::home_capture) fn admit(
        read: Arc<OfflineReadAdmission>,
        store: Store,
        medium: Arc<dyn OwnedMediumOutput>,
        cancellation: ServiceCancellation,
    ) -> io::Result<Arc<Self>> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(refused("Original output requires the root boot controller"));
        }
        let cancelled = AtomicBool::new(false);
        read.revalidate(&cancelled)?;
        let plan = read.output_selection_declaration(&cancelled)?;
        // Original read admission must retain the full root-protected
        // selection plus every exact readonly system-root projection. Only the
        // real paired medium implementation can accept this nonserializable proof.
        let applications = read.admit_application_output(&cancelled)?;
        if let Some(proof) = &applications {
            proof.validate_for(&read, &cancelled)?;
            medium.admit_application_output(proof)?;
        }
        medium.check()?;
        let binding = medium.mount_binding()?;
        if binding.operation != plan.source().operation_id
            || binding.read_claim_digest != read.output_read_claim_digest(&cancelled)?
            || binding.reserve_bytes != plan.sealed_reservation()
            || binding.original_uid != plan.requested_caller().uid()
        {
            return Err(refused("Original output pending transaction differs"));
        }
        store.check_original_output_operation(&binding.operation)?;
        let files = medium.files()?;
        read.check_output_medium(&files.disk, &cancelled)?;
        validate_partition(&files, plan.media())?;
        crate::setup::migration_handoff::validate_offline_source_disks(
            &read.output_source_plans(&cancelled)?,
            files.disk.metadata()?.rdev(),
        )?;
        store.check_original_output_filesystem(files.partition.metadata()?.rdev())?;
        let mut capacity: libc::statvfs = unsafe { std::mem::zeroed() };
        use std::os::fd::AsRawFd;
        if unsafe { libc::fstatvfs(files.output.as_raw_fd(), &raw mut capacity) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let free = capacity
            .f_bavail
            .checked_mul(capacity.f_frsize)
            .ok_or_else(|| refused("Original output actual capacity overflow"))?;
        if binding.reserve_bytes
            > crate::setup::portable_home_media::payload_capacity_for_free_bytes(free)?
        {
            return Err(refused(
                "Original output lacks payload and metadata capacity",
            ));
        }
        let digest = binding_digest(&binding)?;
        let (intent, intent_bytes) =
            store.claim_prepared_original_output_intent(digest, &binding.attempt, &files.output)?;
        let transaction = Arc::new(Self {
            read,
            applications,
            store: Mutex::new(store),
            intent,
            intent_bytes,
            medium,
            binding,
            cancellation,
            cancelled,
        });
        transaction.check()?;
        // Establish working-space capacity before recording any worker launch.
        let _ = transaction.medium.inventory_scratch()?;
        // Independent actual block/mount/UUID/overlap checks occur before
        // returning authority. No writer exists and incomplete intent alone
        // cannot publish or complete an export.
        let _ = OriginalOutputAdmission::admit(transaction.clone())?;
        Ok(transaction)
    }
    pub fn with_writer<T>(
        self: Arc<Self>,
        runtime: &Path,
        publisher: &str,
        run: impl FnOnce(CaptureAdmission, super::OriginalReaderRequest, [u8; 32]) -> io::Result<T>,
    ) -> io::Result<T> {
        self.check()?;
        let proof = OriginalOutputAdmission::admit(self.clone())?;
        // Export and verification callers persist their distinct checkpoints
        // before entering this shared output-capability wrapper.
        let result =
            self.with_inventory_reader(runtime, publisher, |admission, request, digest| {
                run(admission.with_original_output(proof)?, request, digest)
            });
        self.check()?;
        result
    }
    /// Review uses the same admitted medium working space as export, retaining
    /// its actual mount lease through supervision without granting output writes.
    pub fn with_inventory_reader<T>(
        self: &Arc<Self>,
        runtime: &Path,
        publisher: &str,
        run: impl FnOnce(CaptureAdmission, super::OriginalReaderRequest, [u8; 32]) -> io::Result<T>,
    ) -> io::Result<T> {
        self.check()?;
        let (directory, allowance) = self.medium.inventory_scratch()?;
        let medium = self.medium.clone();
        let result = self.read.with_original_reader(
            runtime,
            publisher,
            &self.cancelled,
            |admission, request, digest| {
                run(
                    admission
                        .with_inventory_scratch(directory, allowance)?
                        .retain_inventory_scratch_custody(medium)?,
                    request,
                    digest,
                )
            },
        );
        self.check()?;
        result
    }
}
impl ServiceAuthority for OfflineOutputTransaction {
    fn check(&self) -> io::Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.read.revalidate(&self.cancelled)?;
        if let Some(proof) = &self.applications {
            proof.validate_for(&self.read, &self.cancelled)?;
        }
        self.medium.check()?;
        let store = self
            .store
            .lock()
            .map_err(|_| refused("Original output Store lock failed"))?;
        store.check_original_output_operation(&self.binding.operation)?;
        store.check_original_output_filesystem(self.binding.partition_rdev)?;
        store.check_original_output_intent(&self.intent, &self.intent_bytes)?;
        drop(store);
        if self.medium.mount_binding()? != self.binding
            || self.read.output_read_claim_digest(&self.cancelled)?
                != self.binding.read_claim_digest
        {
            return Err(refused("Original output authority changed"));
        }
        let plan = self.read.output_selection_declaration(&self.cancelled)?;
        let files = self.medium.files()?;
        use sha2::Digest;
        let digest: [u8; 32] = sha2::Sha256::digest(serde_json::to_vec(plan.media())?).into();
        if self.binding.medium_claim_digest != digest {
            return Err(refused(
                "Original output authenticated medium binding differs",
            ));
        }
        // Reattest the actual target FD and GPT from authenticated ReadAdmission,
        // not a target UUID or filesystem directory chosen by the caller.
        self.read
            .check_output_medium(&files.disk, &self.cancelled)?;
        validate_partition(&files, plan.media())?;
        crate::setup::migration_handoff::validate_offline_source_disks(
            &self.read.output_source_plans(&self.cancelled)?,
            files.disk.metadata()?.rdev(),
        )?;
        Ok(())
    }
}
impl OriginalOutputCustody for OfflineOutputTransaction {
    fn held_files(&self) -> io::Result<OriginalOutputFiles> {
        self.check()?;
        let mut files = self.medium.files()?;
        // A mount implementation must never choose the source: clone it only
        // from the already authenticated protected original source custody.
        files.source = self.read.output_original_home(&self.cancelled)?;
        Ok(files)
    }
    fn binding(&self) -> io::Result<OriginalOutputBinding> {
        self.check()?;
        Ok(self.binding.clone())
    }
}
fn number(file: &File) -> io::Result<String> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_block_device() {
        return Err(refused("Original medium custody is not a block descriptor"));
    }
    Ok(format!(
        "{}:{}",
        libc::major(metadata.rdev()),
        libc::minor(metadata.rdev())
    ))
}
/// Current supported topology: exact ordinary disk with a direct partition.
/// Kernel-held identities and current bounded inventory must agree with GPT.
fn validate_partition(
    files: &OriginalOutputFiles,
    expected: &MediaPartitionBinding,
) -> io::Result<()> {
    let disk_number = number(&files.disk)?;
    let partition_number = number(&files.partition)?;
    let bytes = block_devices::Lsblk::new("NAME,TYPE,MAJ:MIN,SIZE,START,PTUUID,PARTUUID")
        .full_paths(true)
        .output()
        .ok_or_else(|| refused("Original medium block ancestry is unavailable"))?;
    let devices = block_devices::lsblk::parse_slice(&bytes)
        .ok_or_else(|| refused("Original medium block ancestry is invalid"))?;
    let matches = devices
        .iter()
        .filter(|disk| disk.kind == "disk" && disk.maj_min.as_deref() == Some(disk_number.as_str()))
        .collect::<Vec<_>>();
    let [disk] = matches.as_slice() else {
        return Err(refused("Original medium parent disk is ambiguous"));
    };
    let matches = disk
        .children
        .iter()
        .filter(|part| {
            part.kind == "part" && part.maj_min.as_deref() == Some(partition_number.as_str())
        })
        .collect::<Vec<_>>();
    let [part] = matches.as_slice() else {
        return Err(refused("Original medium direct partition is ambiguous"));
    };
    let bytes = expected
        .last_lba
        .checked_sub(expected.first_lba)
        .and_then(|size| size.checked_add(1))
        .and_then(|size| size.checked_mul(expected.sector_bytes))
        .ok_or_else(|| refused("Original medium partition geometry overflow"))?;
    if expected.sector_bytes != 512
        || part.start != Some(expected.first_lba)
        || part.size != bytes
        || !part.children.is_empty()
        || disk
            .ptuuid
            .as_deref()
            .is_none_or(|value| !value.eq_ignore_ascii_case(&expected.disk_guid))
        || part
            .partuuid
            .as_deref()
            .is_none_or(|value| !value.eq_ignore_ascii_case(&expected.partition_guid))
    {
        return Err(refused("Original medium exact partition geometry differs"));
    }
    Ok(())
}

fn binding_digest(binding: &OriginalOutputBinding) -> io::Result<[u8; 32]> {
    use sha2::Digest;
    // Closed, bounded host facts; never a serializer for redeemable authority.
    let bytes = serde_json::to_vec(&(
        1u32,
        binding.read_claim_digest,
        binding.medium_claim_digest,
        &binding.operation,
        &binding.attempt,
        binding.source,
        binding.output,
        binding.original_uid,
        binding.disk_rdev,
        binding.partition_rdev,
        binding.mount_id,
        binding.mount_root,
        binding.filesystem_uuid,
        binding.reserve_bytes,
    ))?;
    if bytes.len() > 4096 {
        return Err(refused("Original output intent exceeds its bound"));
    }
    Ok(sha2::Sha256::digest(bytes).into())
}

pub(in crate::setup::home_capture) use native::review_filesystem_witness;
