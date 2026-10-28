//! Same-user ciphertext handoff. Activation belongs exclusively to Tilo first use.
use super::{
    journey::prepared_media::PreparedMedia,
    migration::{PinnedDirectory, changed, check_cancel},
    migration_durability as durable,
    nofollow::metadata_nofollow,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub schema: u32,
    pub identity_public_key: [u8; 32],
    pub setup: crate::model::PreparedSetupExpectation,
    pub browser: Option<super::browser_transfer::Expectation>,
}

struct Publication {
    files: Vec<(Arc<PinnedDirectory>, String, durable::Identity)>,
    directories: Vec<(Arc<PinnedDirectory>, String, Arc<PinnedDirectory>)>,
    committed: bool,
}
impl Publication {
    fn directory(
        &mut self,
        parent: &Arc<PinnedDirectory>,
        name: &str,
        private: bool,
    ) -> io::Result<Arc<PinnedDirectory>> {
        let directory = match metadata_nofollow(&parent.file, Path::new(name)) {
            Ok(metadata) => parent.open_child(name.as_ref(), &metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let directory = parent.create_child(name.as_ref())?;
                self.directories
                    .push((Arc::clone(parent), name.into(), Arc::clone(&directory)));
                directory
            }
            Err(error) => return Err(error),
        };
        directory.validate_destination()?;
        let storage = storage(&directory)?;
        storage.validate_namespace_control()?;
        if private {
            storage.validate_privacy(Privacy::OwnerOnly)?;
        }
        Ok(directory)
    }
    fn publish(
        &mut self,
        directory: &Arc<PinnedDirectory>,
        name: &str,
        bytes: &[u8],
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_cancel(cancel)?;
        if existing(
            directory,
            name,
            bytes.len() as u64,
            Sha256::digest(bytes).into(),
            cancel,
        )? {
            return Ok(());
        }
        let result = storage(directory)?.publish(
            name.as_ref(),
            bytes,
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        );
        // Publication may have succeeded before a durability barrier failed.
        // Track only an admitted matching result, never another file to clean up.
        if existing(
            directory,
            name,
            bytes.len() as u64,
            Sha256::digest(bytes).into(),
            &AtomicBool::new(false),
        )? {
            let file = storage(directory)?.open_read(name.as_ref(), Privacy::OwnerOnly)?;
            self.files.push((
                Arc::clone(directory),
                name.into(),
                durable::identity(&file)?,
            ));
        }
        result.map_err(io::Error::other)?;
        directory.validate_destination()
    }
}
impl Drop for Publication {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for (parent, name, identity) in self.files.iter().rev() {
            if parent.validate().is_ok()
                && storage(parent)
                    .and_then(|storage| storage.open_read(name.as_ref(), Privacy::OwnerOnly))
                    .and_then(|file| durable::identity(&file))
                    .is_ok_and(|actual| &actual == identity)
            {
                let _ = cap_primitives::fs::remove_file(&parent.file, Path::new(name));
                let _ = durable::sync_directory(&parent.file);
            }
        }
        for (parent, name, directory) in self.directories.iter().rev() {
            if directory.validate().is_ok() {
                let _ = cap_primitives::fs::remove_dir(&parent.file, Path::new(name));
                let _ = durable::sync_directory(&parent.file);
            }
        }
    }
}
fn storage(directory: &PinnedDirectory) -> io::Result<StorageDirectory> {
    StorageDirectory::from_file(durable::readable_directory(&directory.file)?)
}
fn existing(
    directory: &PinnedDirectory,
    name: &str,
    length: u64,
    digest: [u8; 32],
    cancel: &AtomicBool,
) -> io::Result<bool> {
    directory.validate_destination()?;
    let mut file = match storage(directory)?.open_read(name.as_ref(), Privacy::OwnerOnly) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let before = file.metadata()?;
    if !before.is_file() || before.len() != length || before.nlink() != 1 {
        return Err(changed("Existing desktop preparation differs"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut bytes = 0u64;
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .filter(|bytes| *bytes <= length)
            .ok_or_else(|| changed("Desktop preparation changed during verification"))?;
        hash.update(&buffer[..count]);
    }
    let current = storage(directory)?.open_read(name.as_ref(), Privacy::OwnerOnly)?;
    if bytes != length
        || <[u8; 32]>::from(hash.finalize()) != digest
        || durable::identity(&current)? != durable::identity(&file)?
        || stamp(&before) != stamp(&file.metadata()?)
        || stamp(&before) != stamp(&current.metadata()?)
    {
        return Err(changed(
            "Existing desktop preparation differs; it was not replaced",
        ));
    }
    directory.validate_destination()?;
    Ok(true)
}
fn stamp(metadata: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

/// Publish the already authenticated preparation without changing the current
/// account, password, device key, documents or application data.
pub(crate) fn deliver(
    prepared: Arc<PreparedMedia>,
    target_home: &Path,
    cancel: &AtomicBool,
) -> io::Result<Receipt> {
    check_cancel(cancel)?;
    let prepared = prepared.refreshed_for_transfer(cancel)?;
    let manifest = prepared.desktop_manifest()?;
    #[cfg(target_os = "linux")]
    if manifest.original_copy.is_some() {
        return Err(changed(
            "Original source copy requires the installed source reader",
        ));
    }
    if manifest.portable_home.is_some()
        || manifest.portable_flatpak.is_some()
        || manifest.retained_home.is_some()
    {
        return Err(changed(
            "Desktop preparation must leave existing files and applications in place",
        ));
    }
    let artifact = prepared.staging_artifact()?;
    let caller = super::staging::prepared_setup::caller_uid()?;
    // Desktop delivery is never an elevated transaction or a cross-user import.
    // SAFETY: observes credentials only.
    if caller != rustix::process::geteuid().as_raw() {
        return Err(changed("Desktop preparation requires the current account"));
    }
    let home = PinnedDirectory::open_absolute(target_home)?;
    home.validate_destination()?;
    if home.file.metadata()?.uid() != caller {
        return Err(changed("Desktop destination belongs to another account"));
    }
    storage(&home)?.validate_namespace_control()?;
    let setup = artifact.read(caller)?;
    let browser = artifact
        .browser
        .as_ref()
        .map(|browser| browser.read(caller))
        .transpose()?;
    let mut publication = Publication {
        files: Vec::new(),
        directories: Vec::new(),
        committed: false,
    };
    let mut directory = Arc::clone(&home);
    for name in [".local", "state", "tilo"] {
        directory = publication.directory(&directory, name, false)?;
    }
    let lease = storage(&directory)?
        .try_acquire_lease(
            "desktop-delivery.lock".as_ref(),
            tilo_files::native::atomic::lease::LeaseMode::Exclusive,
        )
        .map_err(io::Error::other)?;
    let (setup_root, setup_receipt) = publish_payloads(
        &mut publication,
        &directory,
        &artifact,
        &setup,
        browser.as_deref(),
        cancel,
    )?;
    home.validate()?;
    let sentinel =
        storage(&directory)?.open_read("desktop-delivery.lock".as_ref(), Privacy::Inherit)?;
    if tilo_files::native::atomic::FileIdentity::of(&sentinel)? != lease.identity()? {
        return Err(changed("Desktop publication lease was replaced"));
    }
    check_cancel(cancel)?;
    prepared.desktop_manifest()?;
    // The final receipt is the first-use consumer's readiness marker.
    publication.publish(&setup_root, "receipt.json", &setup_receipt, cancel)?;
    prepared.desktop_manifest()?;
    publication.committed = true;
    Ok(Receipt {
        schema: 1,
        identity_public_key: manifest.identity_public_key,
        setup: artifact.expectation,
        browser: artifact.browser.map(|browser| browser.expectation),
    })
}

fn publish_payloads(
    publication: &mut Publication,
    directory: &Arc<PinnedDirectory>,
    artifact: &super::staging::prepared_setup::PreparedArtifact,
    setup: &[u8],
    browser: Option<&[u8]>,
    cancel: &AtomicBool,
) -> io::Result<(Arc<PinnedDirectory>, Vec<u8>)> {
    let setup_root = publication.directory(directory, "prepared-setup", true)?;
    let browser_root = artifact
        .browser
        .as_ref()
        .map(|_| publication.directory(directory, "prepared-browser", true))
        .transpose()?;
    let setup_receipt = serde_json::to_vec(&artifact.expectation)?;
    // Verify every existing destination before publishing anything new.
    existing(
        &setup_root,
        "prepared-setup.sealed",
        setup.len() as u64,
        Sha256::digest(setup).into(),
        cancel,
    )?;
    existing(
        &setup_root,
        "receipt.json",
        setup_receipt.len() as u64,
        Sha256::digest(&setup_receipt).into(),
        cancel,
    )?;
    if let (Some(artifact), Some(bytes), Some(root)) = (&artifact.browser, browser, &browser_root) {
        let receipt = serde_json::to_vec(&artifact.expectation)?;
        existing(
            root,
            &artifact.expectation.filename,
            bytes.len() as u64,
            Sha256::digest(bytes).into(),
            cancel,
        )?;
        existing(
            root,
            "expectation.json",
            receipt.len() as u64,
            Sha256::digest(&receipt).into(),
            cancel,
        )?;
        publication.publish(root, &artifact.expectation.filename, bytes, cancel)?;
        publication.publish(root, "expectation.json", &receipt, cancel)?;
    }
    publication.publish(&setup_root, "prepared-setup.sealed", setup, cancel)?;
    Ok((setup_root, setup_receipt))
}

#[cfg(test)]
mod tests;
