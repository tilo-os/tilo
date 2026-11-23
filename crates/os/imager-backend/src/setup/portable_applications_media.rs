//! Ciphertext-only selected application delivery on the existing pinned medium.
use super::{MediaGrant, Temporary, ciphertext_digest};
use crate::setup::{
    flatpak_transfer::portable::Expectation,
    migration::{changed, check_cancel},
    migration_durability as durable,
    portable_transfer::ImportReview,
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use std::{
    io::{self, Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
};
impl MediaGrant {
    /// Publish the canonical application receipt only for the exact already
    /// exported ciphertext. Actual stream authentication and namespace death
    /// are enforced by the paired original-output caller before entering here.
    pub fn publish_applications_expectation(
        &self,
        expected: &Expectation,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        expected.validate()?;
        check_cancel(cancel)?;
        self.pin.validate_destination()?;
        if self.application_allocation(expected, cancel)? != 0 {
            return Err(changed(
                "Verified original application ciphertext is missing",
            ));
        }
        self.publish_app_metadata(
            &format!("{}.receipt.json", expected.operation),
            &serde_json::to_vec(expected)?,
            cancel,
        )?;
        self.pin.validate_destination()?;
        check_cancel(cancel)
    }

    /// Verify an already published immutable ciphertext before excluding it
    /// from the next write's allocation budget.
    pub fn application_allocation(
        &self,
        expected: &Expectation,
        cancel: &AtomicBool,
    ) -> io::Result<u64> {
        expected.validate()?;
        self.pin.validate_destination()?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let path = Path::new(&expected.filename);
        match cap_primitives::fs::open(&self.pin.file, path, &options) {
            Ok(mut file) => {
                if !file.metadata()?.is_file()
                    || file.metadata()?.len() != expected.sealed_bytes
                    || ciphertext_digest(&mut file, cancel)? != expected.ciphertext_sha256
                {
                    return Err(changed(
                        "Existing application package differs; it was not replaced",
                    ));
                }
                let current = cap_primitives::fs::open(&self.pin.file, path, &options)?;
                if durable::identity(&current)? != durable::identity(&file)? {
                    return Err(changed("Application ciphertext publication was replaced"));
                }
                self.pin.validate_destination()?;
                Ok(0)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(expected.sealed_bytes),
            Err(error) => Err(error),
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn deliver_applications(
        &self,
        archive: &mut ImportReview,
        expected: &Expectation,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.deliver_applications_reporting(archive, expected, cancel, &mut |_| Ok(()))
    }
    pub fn deliver_applications_reporting(
        &self,
        archive: &mut ImportReview,
        expected: &Expectation,
        cancel: &AtomicBool,
        observer: &mut super::portable_transfer::Observer<'_>,
    ) -> io::Result<()> {
        expected.validate()?;
        archive.verify_held_ciphertext(cancel)?;
        if archive.ciphertext_digest() != expected.ciphertext_sha256
            || archive.info().sealed_bytes != expected.sealed_bytes
        {
            return Err(changed("Prepared application ciphertext changed"));
        }
        self.pin.validate_destination()?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        match cap_primitives::fs::open(&self.pin.file, Path::new(&expected.filename), &options) {
            Ok(_) => {
                if self.application_allocation(expected, cancel)? != 0 {
                    return Err(changed(
                        "Application publication disappeared during delivery",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut temporary = Temporary::create(&self.pin)?;
                if archive.copy_ciphertext_reporting(&mut temporary.file, cancel, observer)?
                    != expected.ciphertext_sha256
                {
                    return Err(changed("Application ciphertext readback failed"));
                }
                temporary.publish(&expected.filename)?;
            }
            Err(error) => return Err(error),
        }
        let receipt = serde_json::to_vec(expected)?;
        self.publish_app_metadata(
            &format!("{}.receipt.json", expected.operation),
            &receipt,
            cancel,
        )?;
        self.pin.validate_destination()
    }
    pub fn publish_applications_selection(
        &self,
        setup: &crate::setup::media_setup::DeliveryReceipt,
        expected: &Expectation,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        expected.validate()?;
        let directory = match self
            .pin
            .create_child(std::ffi::OsStr::new(".tilo-installer"))
        {
            Ok(pin) => pin,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = crate::setup::nofollow::metadata_nofollow(
                    &self.pin.file,
                    Path::new(".tilo-installer"),
                )?;
                self.pin
                    .open_child(std::ffi::OsStr::new(".tilo-installer"), &metadata)?
            }
            Err(error) => return Err(error),
        };
        let scoped = MediaGrant::from_reviewed_pin(directory)?;
        let value = serde_json::json!({"schema":1,"prepared_setup": setup.expectation(),"applications":expected});
        scoped.publish_app_metadata(
            "applications-selection.json",
            &serde_json::to_vec(&value)?,
            cancel,
        )
    }
    fn publish_app_metadata(
        &self,
        name: &str,
        bytes: &[u8],
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if bytes.len() > 18 * 1024 {
            return Err(changed("Application selection exceeds transport bounds"));
        }
        self.pin.validate_destination()?;
        check_cancel(cancel)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        match cap_primitives::fs::open(&self.pin.file, Path::new(name), &options) {
            Ok(file) => {
                if !file.metadata()?.is_file() || file.metadata()?.len() > 18 * 1024 {
                    return Err(changed("Invalid existing application receipt"));
                }
                let mut existing = Vec::new();
                file.take(18 * 1024 + 1).read_to_end(&mut existing)?;
                if existing != bytes {
                    return Err(changed(
                        "Existing application receipt differs; it was not replaced",
                    ));
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut temporary = Temporary::create(&self.pin)?;
                temporary.file.write_all(bytes)?;
                temporary.publish(name)
            }
            Err(error) => Err(error),
        }
    }
}
