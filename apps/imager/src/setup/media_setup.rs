//! Small prepared identity/account setup on a verified writable media filesystem.
//! This never allocates a partition or writes raw tail slack.
use crate::{
    model::TargetDrive,
    setup::{
        migration::{PinnedDirectory, changed, check_cancel},
        migration_durability as durability,
        portable_transfer::{ImportReview, Limits, PackageInfo, Purpose},
    },
};
use block_devices::{BlockDevice, Lsblk};
use cap_fs_ext::OpenOptionsFollowExt;
use cap_primitives::fs::{FollowSymlinks, MetadataExt, OpenOptions};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
pub const DIRECTORY: &str = ".tilo-installer";
pub const FILE: &str = "prepared-setup.sealed";
pub const RECEIPT_FILE: &str = "receipt.json";
const MAX_PLAINTEXT: u64 = 32 * 1024 * 1024;
const COLUMNS: &str = "NAME,TYPE,SIZE,RO,FSTYPE,UUID,PARTUUID,PARTTYPE,MAJ:MIN,MOUNTPOINTS";
pub(crate) mod automatic;
#[cfg(target_os = "linux")]
pub(in crate::setup) mod protected_output;
#[cfg(target_os = "linux")]
pub(crate) use protected_output::{provision_after_write, validate_pending_caller};
#[cfg(target_os = "linux")]
pub(in crate::setup) mod boot_medium;
#[cfg(target_os = "linux")]
pub(in crate::setup) mod boot_review;
#[cfg(target_os = "linux")]
pub(in crate::setup) use super::prepared_setup::completion::CompletedSetupReceipt;
mod existing;
pub(in crate::setup) mod inbox;
#[derive(Clone, Debug, PartialEq, Eq)]
struct PartitionBinding {
    name: String,
    filesystem: String,
    uuid: String,
    partition_uuid: String,
    number: String,
}
/// A filesystem grant belongs to the already reviewed media device, and pins
/// its exact mounted root. Arbitrary host paths and stale partition lists fail.
pub struct MediaFilesystemGrant {
    target: TargetDrive,
    root: PathBuf,
    pin: Arc<PinnedDirectory>,
    partition: PartitionBinding,
}
impl MediaFilesystemGrant {
    /// The binding is only an expectation: verify it against the held writer
    /// descriptor and this actual filesystem grant before delivering anything.
    pub(in crate::setup) fn validate_offline_media(
        &self,
        raw: &std::fs::File,
        expected: &crate::media_layout::MediaPartitionBinding,
    ) -> io::Result<()> {
        self.validate()?;
        crate::write::attest_target_descriptor(&self.target, raw)?;
        if crate::media_layout::binding(raw, self.target.size, &expected.target_stable_id)?
            != *expected
            || self.target.stable_id.as_deref() != Some(expected.target_stable_id.as_str())
            || !self
                .partition
                .partition_uuid
                .eq_ignore_ascii_case(&expected.partition_guid)
        {
            return Err(changed(
                "Offline export medium differs from authenticated selection",
            ));
        }
        self.validate()
    }
    pub(in crate::setup) fn portable_home_grant(
        &self,
    ) -> io::Result<super::portable_home_media::MediaGrant> {
        self.validate()?;
        super::portable_home_media::MediaGrant::from_reviewed_pin(Arc::clone(&self.pin))
    }
    /// Does not mount, format, repartition, or start writing. The existing writer
    /// must first obtain a writable mounted filesystem on its selected medium.
    pub fn resolve(target: &TargetDrive, mounted_root: &Path) -> io::Result<Self> {
        if !target.is_media_target() {
            return Err(changed(
                "Prepared setup requires the reviewed external medium",
            ));
        }
        crate::write::revalidate_target(target)?;
        let pin = PinnedDirectory::open_absolute(mounted_root)?;
        pin.validate_destination()?;
        writable(&pin)?;
        let partition = select_partition(&inventory()?, target, mounted_root, pin.identity.dev())?;
        Ok(Self {
            target: target.clone(),
            root: mounted_root.to_owned(),
            pin,
            partition,
        })
    }
    fn validate(&self) -> io::Result<()> {
        crate::write::revalidate_target(&self.target)?;
        self.pin.validate_destination()?;
        writable(&self.pin)?;
        if select_partition(
            &inventory()?,
            &self.target,
            &self.root,
            self.pin.identity.dev(),
        )? != self.partition
        {
            return Err(changed("Prepared setup filesystem/device binding changed"));
        }
        Ok(())
    }
}
fn inventory() -> io::Result<Vec<BlockDevice>> {
    Lsblk::new(COLUMNS)
        .full_paths(true)
        .list()
        .ok_or_else(|| changed("Cannot identify the selected media filesystem"))
}
fn select_partition(
    devices: &[BlockDevice],
    target: &TargetDrive,
    root: &Path,
    device: u64,
) -> io::Result<PartitionBinding> {
    let number = format!("{}:{}", libc::major(device), libc::minor(device));
    let mut matches = Vec::new();
    for disk in devices.iter().filter(|disk| {
        disk.name == target.dev_path()
            && disk.kind == "disk"
            && disk.size == target.size
            && !disk.read_only
    }) {
        for partition in &disk.children {
            if partition.kind != "part"
                || partition.read_only
                || !partition.children.is_empty()
                || partition.maj_min.as_deref() != Some(number.as_str())
                || !partition
                    .all_mountpoints()
                    .iter()
                    .any(|point| Path::new(point) == root)
            {
                continue;
            }
            let filesystem = partition
                .fstype
                .as_deref()
                .filter(|kind| matches!(*kind, "vfat" | "exfat" | "ext4" | "xfs" | "btrfs"))
                .ok_or_else(|| {
                    changed("Media has no supported writable customization filesystem")
                })?;
            let uuid = partition
                .uuid
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| changed("Media filesystem identity is missing"))?;
            let partition_uuid = partition
                .partuuid
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| changed("Media partition identity is missing"))?;
            matches.push(PartitionBinding {
                name: partition.name.clone(),
                filesystem: filesystem.into(),
                uuid: uuid.into(),
                partition_uuid: partition_uuid.into(),
                number: number.clone(),
            });
        }
    }
    match matches.as_slice() {
        [only] => Ok(only.clone()),
        _ => Err(changed(
            "Prepared setup needs an explicit mounted writable filesystem on this reviewed medium; no raw-tail fallback is permitted",
        )),
    }
}
fn writable(pin: &PinnedDirectory) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: valid held descriptor and writable statvfs storage.
    if unsafe { libc::fstatvfs(pin.file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful syscall initialized status.
    if unsafe { status.assume_init() }.f_flag & libc::ST_RDONLY != 0 {
        return Err(changed("Granted media filesystem is read-only"));
    }
    Ok(())
}
/// Holds a fully authenticated bounded PreparedSetup package, never a password.
pub struct PreparedSetupDelivery {
    package: ImportReview,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct DeliveryReceipt {
    pub relative_path: PathBuf,
    pub sealed_bytes: u64,
    pub ciphertext_sha256: String,
}
impl DeliveryReceipt {
    /// Nonsecret delivery expectation. It binds bytes, never authorizes adoption.
    pub fn expectation(&self) -> serde_json::Value {
        serde_json::json!({"schema":1,"sealed_bytes":self.sealed_bytes,
            "ciphertext_sha256":self.ciphertext_sha256})
    }
}
impl PreparedSetupDelivery {
    pub fn review(package: &Path, password: &[u8], cancel: &AtomicBool) -> io::Result<Self> {
        let package = ImportReview::review(
            package,
            password,
            Purpose::PreparedSetup,
            Limits {
                plaintext_bytes: MAX_PLAINTEXT,
                entries: 2048,
                metadata_bytes: 2 * 1024 * 1024,
            },
            cancel,
        )?;
        Ok(Self { package })
    }
    pub fn info(&self) -> &PackageInfo {
        self.package.info()
    }
    pub(in crate::setup) fn verify_reviewed_ciphertext(
        &mut self,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.package.verify_held_ciphertext(cancel)
    }
    pub(in crate::setup) fn copy_preparation_ciphertext(
        &mut self,
        output: &mut std::fs::File,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.package.copy_ciphertext(output, cancel)?;
        output.sync_all()
    }
    /// Bind the public profile to the authenticated ciphertext. Delivery still
    /// independently verifies that the held bytes match this expectation.
    pub fn expectation(&self) -> crate::model::PreparedSetupExpectation {
        crate::model::PreparedSetupExpectation {
            schema: 1,
            sealed_bytes: self.package.info().sealed_bytes,
            ciphertext_sha256: hex::encode(self.package.ciphertext_digest()),
        }
    }
    pub fn deliver(
        &mut self,
        grant: &MediaFilesystemGrant,
        cancel: &AtomicBool,
    ) -> io::Result<DeliveryReceipt> {
        grant.validate()?;
        let receipt =
            deliver_ciphertext_reporting(&mut self.package, &grant.pin, cancel, &mut |_| Ok(()))?;
        grant.validate()?;
        Ok(receipt)
    }

    pub(in crate::setup) fn deliver_protected_inbox_reporting(
        &mut self,
        grant: &inbox::ProtectedInboxGrant<'_>,
        cancel: &AtomicBool,
        observer: &mut super::portable_transfer::Observer<'_>,
    ) -> io::Result<DeliveryReceipt> {
        grant.validate()?;
        let root = grant.pin();
        let receipt = match existing::verify(&mut self.package, root, cancel)? {
            Some(receipt) => receipt,
            None => deliver_ciphertext_reporting(&mut self.package, root, cancel, observer)?,
        };
        grant.validate()?;
        Ok(receipt)
    }

    /// Retry automatic delivery without replacing an identical completed copy.
    /// Existing ciphertext and receipt must both independently match this review.
    pub(in crate::setup) fn deliver_or_verify_reporting(
        &mut self,
        grant: &MediaFilesystemGrant,
        cancel: &AtomicBool,
        observer: &mut super::portable_transfer::Observer<'_>,
    ) -> io::Result<DeliveryReceipt> {
        grant.validate()?;
        let result = existing::verify(&mut self.package, &grant.pin, cancel)?;
        let receipt = if let Some(receipt) = result {
            receipt
        } else {
            deliver_ciphertext_reporting(&mut self.package, &grant.pin, cancel, observer)?
        };
        grant.validate()?;
        Ok(receipt)
    }
}
fn delivery_directory(root: &Arc<PinnedDirectory>) -> io::Result<Arc<PinnedDirectory>> {
    use cap_primitives::fs::{DirBuilderExt, DirOptions};
    root.validate_destination()?;
    match crate::setup::nofollow::metadata_nofollow(&root.file, Path::new(DIRECTORY)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut options = DirOptions::new();
            // Root helper owns a protected custody namespace. This applies only
            // to fresh creation; existing nonroot/0755 trees are never repaired.
            #[cfg(unix)]
            let mode = if rustix::process::geteuid().as_raw() == 0 {
                0o700
            } else {
                0o755
            };
            #[cfg(not(unix))]
            let mode = 0o755;
            options.mode(mode);
            cap_primitives::fs::create_dir(&root.file, Path::new(DIRECTORY), &options)?;
        }
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let metadata = crate::setup::nofollow::metadata_nofollow(&root.file, Path::new(DIRECTORY))?;
    let directory = root.open_child(std::ffi::OsStr::new(DIRECTORY), &metadata)?;
    directory.validate_destination()?;
    Ok(directory)
}
#[cfg(test)]
fn deliver_ciphertext(
    package: &mut ImportReview,
    root: &Arc<PinnedDirectory>,
    cancel: &AtomicBool,
) -> io::Result<DeliveryReceipt> {
    deliver_ciphertext_reporting(package, root, cancel, &mut |_| Ok(()))
}
fn deliver_ciphertext_reporting(
    package: &mut ImportReview,
    root: &Arc<PinnedDirectory>,
    cancel: &AtomicBool,
    observer: &mut super::portable_transfer::Observer<'_>,
) -> io::Result<DeliveryReceipt> {
    check_cancel(cancel)?;
    let directory = delivery_directory(root)?;
    match crate::setup::nofollow::metadata_nofollow(&directory.file, Path::new(RECEIPT_FILE)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
        Ok(_) => return Err(changed("Prepared setup expectation already exists")),
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    use cap_primitives::fs::OpenOptionsExt;
    options.mode(0o644);
    let mut file = cap_primitives::fs::open(&directory.file, Path::new(FILE), &options)?;
    let mut receipt_created = false;
    let result = (|| {
        let digest = package.copy_ciphertext_reporting(&mut file, cancel, observer)?;
        directory.validate_destination()?;
        let receipt = DeliveryReceipt {
            relative_path: Path::new(DIRECTORY).join(FILE),
            sealed_bytes: package.info().sealed_bytes,
            ciphertext_sha256: hex::encode(digest),
        };
        let bytes = serde_json::to_vec(&receipt.expectation())?;
        let mut receipt_file =
            cap_primitives::fs::open(&directory.file, Path::new(RECEIPT_FILE), &options)?;
        receipt_created = true;
        use std::io::Write as _;
        receipt_file.write_all(&bytes)?;
        durability::sync_file(&receipt_file)?;
        directory.validate_destination()?;
        durability::sync_directory(&directory.file)?;
        durability::sync_directory(&root.file)?;
        Ok(receipt)
    })();
    if result.is_err() {
        let _ = cap_primitives::fs::remove_file(&directory.file, Path::new(FILE));
        if receipt_created {
            let _ = cap_primitives::fs::remove_file(&directory.file, Path::new(RECEIPT_FILE));
        }
    }
    result
}
#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub(in crate::setup) fn deliver_completed_ciphertext(
    delivery: &mut PreparedSetupDelivery,
    root: &Arc<PinnedDirectory>,
    cancel: &AtomicBool,
) -> io::Result<DeliveryReceipt> {
    let receipt = match existing::verify(&mut delivery.package, root, cancel)? {
        Some(receipt) => receipt,
        None => deliver_ciphertext_reporting(&mut delivery.package, root, cancel, &mut |_| Ok(()))?,
    };
    if existing::verify(&mut delivery.package, root, cancel)?.is_none() {
        return Err(changed("Completed setup ciphertext readback missing"));
    }
    Ok(receipt)
}

#[cfg(target_os = "linux")]
pub(in crate::setup) fn verify_completed_ciphertext(
    delivery: &mut PreparedSetupDelivery,
    root: &Arc<PinnedDirectory>,
    cancel: &AtomicBool,
) -> io::Result<Option<DeliveryReceipt>> {
    existing::verify(&mut delivery.package, root, cancel)
}
