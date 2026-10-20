//! Small prepared identity/account setup on a verified writable media filesystem.
//! This never allocates a partition or writes raw tail slack.
pub use crate::prepared_media::{DIRECTORY, DeliveryReceipt, FILE, RECEIPT_FILE};
use crate::prepared_media::{MAX_PLAINTEXT, PreparedMediaHandle};
use crate::{
    imager_model::TargetDrive,
    setup::{
        migration::{PinnedDirectory, changed, check_cancel},
        migration_durability as durability,
        portable_transfer::{ImportReview, Limits, PackageInfo, Purpose},
    },
};
use cap_fs_ext::OpenOptionsFollowExt;
use cap_primitives::fs::{FollowSymlinks, MetadataExt, OpenOptions};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
pub mod automatic;
mod custody;
#[cfg(target_os = "linux")]
pub mod protected_output;
#[cfg(target_os = "linux")]
pub use protected_output::{provision_after_write, validate_pending_caller};
#[cfg(target_os = "linux")]
pub mod boot_medium;
#[cfg(target_os = "linux")]
pub mod boot_review;
#[cfg(target_os = "linux")]
pub use super::prepared_setup::completion::CompletedSetupReceipt;
mod existing;
pub mod inbox;
/// Adapter for frontend archive operations. Device, partition and mounted-root
/// authority belongs exclusively to the backend's opaque grant.
pub struct MediaFilesystemGrant {
    authority: crate::prepared_media::filesystem::MediaFilesystemGrant,
    pin: Arc<PinnedDirectory>,
}
impl MediaFilesystemGrant {
    fn from_backend(
        authority: crate::prepared_media::filesystem::MediaFilesystemGrant,
    ) -> io::Result<Self> {
        let pin = PinnedDirectory::open_absolute(authority.root_path())?;
        authority.check_directory_identity(&pin.file)?;
        Ok(Self { authority, pin })
    }
    pub fn resolve(target: &TargetDrive, mounted_root: &Path) -> io::Result<Self> {
        Self::from_backend(
            crate::prepared_media::filesystem::MediaFilesystemGrant::resolve(target, mounted_root)?,
        )
    }
    fn validate(&self) -> io::Result<()> {
        self.authority.validate()?;
        self.pin.validate_destination()?;
        self.authority.check_directory_identity(&self.pin.file)
    }
    pub fn validate_offline_media(
        &self,
        raw: &std::fs::File,
        expected: &crate::media_layout::MediaPartitionBinding,
    ) -> io::Result<()> {
        self.validate()?;
        self.authority.validate_offline_media(raw, expected)?;
        self.validate()
    }
    pub fn portable_home_grant(&self) -> io::Result<super::portable_home_media::MediaGrant> {
        self.validate()?;
        super::portable_home_media::MediaGrant::from_reviewed_pin(Arc::clone(&self.pin))
    }
    /// Publish the public other-computer descriptor beside the encrypted
    /// package. It contains no path, token or content key, so the medium can
    /// carry it before the destination has a real identity. A retry must find
    /// the same descriptor, never replace an earlier reviewed one.
    pub fn publish_other_device(
        &self,
        handoff: &super::other_device::PublicHandoff,
    ) -> io::Result<()> {
        self.validate()?;
        let directory = delivery_directory(&self.pin)?;
        let bytes = handoff.encode()?;
        let name = super::other_device::DESCRIPTOR_FILE;
        match crate::setup::nofollow::metadata_nofollow(&directory.file, Path::new(name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => {
                let mut existing = Vec::new();
                let mut options = OpenOptions::new();
                options.read(true).follow(FollowSymlinks::No);
                let file = cap_primitives::fs::open(&directory.file, Path::new(name), &options)?;
                use std::io::Read as _;
                file.take(16 * 1024 + 1).read_to_end(&mut existing)?;
                return (existing == bytes)
                    .then_some(())
                    .ok_or_else(|| changed("Other-computer handoff already differs"));
            }
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        use cap_primitives::fs::OpenOptionsExt;
        options.mode(0o644);
        let mut file = cap_primitives::fs::open(&directory.file, Path::new(name), &options)?;
        use std::io::Write as _;
        let result = (|| {
            file.write_all(&bytes)?;
            durability::sync_file(&file)?;
            directory.validate_destination()?;
            durability::sync_directory(&directory.file)?;
            durability::sync_directory(&self.pin.file)
        })();
        if result.is_err() {
            let _ = cap_primitives::fs::remove_file(&directory.file, Path::new(name));
        }
        result
    }
}
/// Holds a fully authenticated bounded PreparedSetup package, never a password.
pub struct PreparedSetupDelivery {
    package: PreparedMediaHandle,
    info: PackageInfo,
}
impl PreparedSetupDelivery {
    pub fn review(package: &Path, password: &[u8], cancel: &AtomicBool) -> io::Result<Self> {
        let reviewed = ImportReview::review(
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
        let info = reviewed.info().clone();
        Ok(Self {
            package: custody::hold(&reviewed, package, password, cancel)?,
            info,
        })
    }
    pub fn info(&self) -> &PackageInfo {
        &self.info
    }
    pub fn verify_reviewed_ciphertext(&mut self, cancel: &AtomicBool) -> io::Result<()> {
        self.package
            .verify_held_ciphertext(cancel)
            .map_err(Into::into)
    }
    pub fn copy_preparation_ciphertext(
        &mut self,
        output: &mut std::fs::File,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.package.copy_ciphertext(output, cancel)?;
        output.sync_all()
    }
    /// Bind the public profile to the authenticated ciphertext. Delivery still
    /// independently verifies that the held bytes match this expectation.
    pub fn expectation(&self) -> crate::imager_model::PreparedSetupExpectation {
        crate::imager_model::PreparedSetupExpectation {
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

    pub fn deliver_protected_inbox_reporting(
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
    pub fn deliver_or_verify_reporting(
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

    /// Keep the encrypted package for another computer under its own
    /// directory. The live installer must not treat it as a same-computer
    /// setup it can authenticate without the first-boot destination.
    pub fn deliver_other_device(
        &mut self,
        grant: &MediaFilesystemGrant,
        cancel: &AtomicBool,
    ) -> io::Result<DeliveryReceipt> {
        grant.validate()?;
        let directory = directory_named(&grant.pin, "other-device")?;
        let receipt = deliver_ciphertext_in(
            &mut self.package,
            &directory,
            &grant.pin,
            Path::new(DIRECTORY).join("other-device"),
            cancel,
            &mut |_| Ok(()),
        )?;
        grant.validate()?;
        Ok(receipt)
    }
}
fn delivery_directory(root: &Arc<PinnedDirectory>) -> io::Result<Arc<PinnedDirectory>> {
    directory_named(root, DIRECTORY)
}

fn directory_named(root: &Arc<PinnedDirectory>, name: &str) -> io::Result<Arc<PinnedDirectory>> {
    use cap_primitives::fs::{DirBuilderExt, DirOptions};
    root.validate_destination()?;
    match crate::setup::nofollow::metadata_nofollow(&root.file, Path::new(name)) {
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
            cap_primitives::fs::create_dir(&root.file, Path::new(name), &options)?;
        }
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let metadata = crate::setup::nofollow::metadata_nofollow(&root.file, Path::new(name))?;
    let directory = root.open_child(std::ffi::OsStr::new(name), &metadata)?;
    directory.validate_destination()?;
    Ok(directory)
}
#[cfg(test)]
fn deliver_ciphertext(
    package: &mut PreparedMediaHandle,
    root: &Arc<PinnedDirectory>,
    cancel: &AtomicBool,
) -> io::Result<DeliveryReceipt> {
    deliver_ciphertext_reporting(package, root, cancel, &mut |_| Ok(()))
}
fn deliver_ciphertext_reporting(
    package: &mut PreparedMediaHandle,
    root: &Arc<PinnedDirectory>,
    cancel: &AtomicBool,
    observer: &mut super::portable_transfer::Observer<'_>,
) -> io::Result<DeliveryReceipt> {
    let directory = delivery_directory(root)?;
    deliver_ciphertext_in(
        package,
        &directory,
        root,
        Path::new(DIRECTORY).to_path_buf(),
        cancel,
        observer,
    )
}

fn deliver_ciphertext_in(
    package: &mut PreparedMediaHandle,
    directory: &Arc<PinnedDirectory>,
    root: &Arc<PinnedDirectory>,
    relative: PathBuf,
    cancel: &AtomicBool,
    observer: &mut super::portable_transfer::Observer<'_>,
) -> io::Result<DeliveryReceipt> {
    check_cancel(cancel)?;
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
        let digest = package.copy_ciphertext_reporting(&mut file, cancel, &mut |progress| {
            observer(super::portable_transfer::ArchiveProgress {
                phase: super::portable_transfer::ArchivePhase::DeliveringCiphertext,
                completed: progress.completed,
                total: progress.total,
            })
        })?;
        directory.validate_destination()?;
        let receipt = DeliveryReceipt {
            relative_path: relative.clone(),
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
pub fn deliver_completed_ciphertext(
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
pub fn verify_completed_ciphertext(
    delivery: &mut PreparedSetupDelivery,
    root: &Arc<PinnedDirectory>,
    cancel: &AtomicBool,
) -> io::Result<Option<DeliveryReceipt>> {
    existing::verify(&mut delivery.package, root, cancel)
}
