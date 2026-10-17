//! Root-protected publication after verified output; immutable pending setup stays intact.
use super::origin::Origin;
use super::*;
use crate::setup::media_setup;
use crate::setup::migration::{changed, check_cancel};
use crate::setup::migration_durability as durability;
use cap_primitives::fs::MetadataExt as _;
use std::ffi::OsStr;
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};

const COMMIT: &str = "completion.json";
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Completion {
    schema: u32,
    attempt: String,
    pending: crate::imager_model::PreparedSetupExpectation,
    setup: crate::imager_model::PreparedSetupExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    home: Option<crate::setup::portable_home_media::Expectation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
}
/// Returned only after authenticated ciphertext readback and durable commit.
/// This is not an installation or unmount receipt.
#[derive(serde::Serialize)]
pub struct CompletedSetupReceipt {
    pub setup: crate::imager_model::PreparedSetupExpectation,
}
pub fn publish(
    package: &mut crate::setup::prepared_setup::completion::CompletedSetupPackage,
    verified: &Origin,
    cancel: &AtomicBool,
) -> io::Result<CompletedSetupReceipt> {
    check_cancel(cancel)?;
    package.validate_completion_origin(verified)?;
    if let Some(receipt) = recover(package, verified, cancel)? {
        return Ok(receipt);
    }
    if package.manifest().completed_home.as_ref() != verified.home()
        || package.manifest().offline_home.is_some()
        || package.manifest().offline_applications.is_some()
        || package.manifest().completed_applications_only != (verified.schema() == 2)
        || package.manifest().portable_flatpak.as_ref() != verified.applications()
    {
        return Err(changed("Completed setup does not bind verified Home"));
    }
    let delivery = &mut package.delivery;
    let expected = delivery.expectation();
    verified.with_completed_directory(|root| {
        root.validate_destination()?;
        // A proven identical delivered package already owns its ciphertext
        // allocation. Require copy capacity only when actual delivery is absent.
        let receipt = if let Some(receipt) =
            media_setup::verify_completed_ciphertext(delivery, &root, cancel)?
        {
            receipt
        } else {
            // This copies ciphertext. Home's inverse plaintext limits
            // subtract framing and are not physical writable capacity.
            let free = super::super::completed_setup_free_bytes(&root.file)?;
            let needed =
                super::super::completed_setup_ciphertext_reservation(expected.sealed_bytes)?;
            if needed > free {
                return Err(changed("Completed setup lacks reserved writable capacity"));
            }
            media_setup::deliver_completed_ciphertext(delivery, &root, cancel)?
        };
        if receipt.sealed_bytes != expected.sealed_bytes
            || receipt.ciphertext_sha256 != expected.ciphertext_sha256
        {
            return Err(changed("Completed setup ciphertext readback differs"));
        }
        verified.validate()?;
        check_cancel(cancel)?;
        let completion = Completion {
            schema: verified.schema(),
            attempt: verified.attempt().to_owned(),
            pending: verified.pending_setup_expectation()?.clone(),
            setup: expected.clone(),
            home: verified.home().cloned(),
            applications: verified.applications().cloned(),
        };
        let bytes = serde_json::to_vec(&completion)?;
        if bytes.len() > 16 * 1024 {
            return Err(changed("Completed setup commit exceeds bounds"));
        }
        let storage = StorageDirectory::from_file(durability::readable_directory(&root.file)?)?;
        storage.validate_privacy(Privacy::OwnerOnly)?;
        // IfAbsent is an immutable completion, never a replacement of
        // pending identity/account ciphertext or a prior generation.
        storage
            .publish(
                OsStr::new(COMMIT),
                &bytes,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .map_err(|error| error.into_io_error())?;
        let mut committed = storage.open_read(OsStr::new(COMMIT), Privacy::OwnerOnly)?;
        use std::io::Read as _;
        let mut found = Vec::new();
        std::io::Read::by_ref(&mut committed)
            .take(16 * 1024 + 1)
            .read_to_end(&mut found)?;
        if found != bytes {
            return Err(changed("Completed setup commit readback differs"));
        }
        root.validate_destination()?;
        verified.commit_completed_setup(&expected, sha2::Sha256::digest(&bytes).into())?;
        verified.validate()?;
        check_cancel(cancel)?;
        Ok(CompletedSetupReceipt {
            setup: expected.clone(),
        })
    })
}

/// Same held genuine transaction may recover a prior atomic publication only
/// after authenticating its full ciphertext and exact preserved inner artifacts.
/// Reconstructing a transaction after reboot remains the canonical Store's job.
fn recover(
    package: &CompletedSetupPackage,
    verified: &Origin,
    cancel: &AtomicBool,
) -> io::Result<Option<CompletedSetupReceipt>> {
    package.validate_completion_origin(verified)?;
    verified.with_completed_directory(|root| {
        check_cancel(cancel)?;
        let storage = StorageDirectory::from_file(durability::readable_directory(&root.file)?)?;
        storage.validate_privacy(Privacy::OwnerOnly)?;
        let mut marker = match storage.open_read(OsStr::new(COMMIT), Privacy::OwnerOnly) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        use std::io::Read as _;
        let before = cap_primitives::fs::Metadata::from_file(&marker)?;
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut marker)
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16 * 1024 || bytes.is_empty() {
            return Err(invalid());
        }
        let committed: Completion = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if committed.schema != verified.schema()
            || committed.attempt != verified.attempt()
            || committed.pending != *verified.pending_setup_expectation()?
            || committed.home.as_ref() != verified.home()
            || committed.applications.as_ref() != verified.applications()
            || committed.applications != package.manifest.portable_flatpak
            || !committed.setup.valid()
            || committed.setup.sealed_bytes > LIMITS.sealed_bound().ok_or_else(invalid)?
        {
            return Err(invalid());
        }
        let metadata = crate::setup::nofollow::metadata_nofollow(
            &root.file,
            Path::new(media_setup::DIRECTORY),
        )?;
        let directory = root.open_child(OsStr::new(media_setup::DIRECTORY), &metadata)?;
        directory.validate_destination()?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let mut source =
            cap_primitives::fs::open(&directory.file, Path::new(media_setup::FILE), &options)?;
        let source_before = cap_primitives::fs::Metadata::from_file(&source)?;
        if !source_before.is_file()
            || source_before.len() != committed.setup.sealed_bytes
            || source_before.uid() != directory.identity.uid()
            || source_before.mode() & 0o022 != 0
            || source_before.nlink() != 1
        {
            return Err(invalid());
        }
        let copy = tempfile::Builder::new()
            .prefix("tilo-completed-recovery-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let path = copy.path().join(media_setup::FILE);
        let pin = PinnedDirectory::open_absolute(copy.path())?;
        let mut create = OpenOptions::new();
        create
            .write(true)
            .create_new(true)
            .mode(0o600)
            .follow(FollowSymlinks::No);
        let mut output =
            cap_primitives::fs::open(&pin.file, Path::new(media_setup::FILE), &create)?;
        let mut total = 0u64;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            check_cancel(cancel)?;
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total = total
                .checked_add(count as u64)
                .filter(|size| *size <= committed.setup.sealed_bytes)
                .ok_or_else(invalid)?;
            hash.update(&buffer[..count]);
            output.write_all(&buffer[..count])?;
        }
        output.sync_all()?;
        if total != committed.setup.sealed_bytes
            || hex::encode(hash.finalize()) != committed.setup.ciphertext_sha256
        {
            return Err(invalid());
        }
        let reviewed = PreparedSetupReview::open(
            &path,
            &package.password,
            &copy.path().join("setup"),
            cancel,
        )?;
        reviewed.require_completed_export()?;
        // The canonical delivered receipt is part of the handoff, not only
        // the encrypted bytes. Missing or conflicting receipts cannot recover.
        let mut recovered_delivery =
            media_setup::PreparedSetupDelivery::review(&path, &package.password, cancel)?;
        let receipt =
            media_setup::verify_completed_ciphertext(&mut recovered_delivery, &root, cancel)?
                .ok_or_else(invalid)?;
        if recovered_delivery.expectation() != committed.setup
            || receipt.sealed_bytes != committed.setup.sealed_bytes
            || receipt.ciphertext_sha256 != committed.setup.ciphertext_sha256
        {
            return Err(invalid());
        }
        if reviewed.manifest() != &package.manifest {
            return Err(invalid());
        }
        for (name, expected) in &package.sealed_artifacts {
            let bytes = bounded_file(&reviewed.pin, name, LIMITS.plaintext_bytes)?;
            if <[u8; 32]>::from(Sha256::digest(bytes)) != *expected {
                return Err(invalid());
            }
        }
        if !crate::setup::migration::same_snapshot(
            &source_before,
            &cap_primitives::fs::Metadata::from_file(&source)?,
        ) || !crate::setup::migration::same_snapshot(
            &source_before,
            &crate::setup::nofollow::metadata_nofollow(
                &directory.file,
                Path::new(media_setup::FILE),
            )?,
        ) || !crate::setup::migration::same_snapshot(
            &before,
            &cap_primitives::fs::Metadata::from_file(&marker)?,
        ) || !crate::setup::migration::same_snapshot(
            &before,
            &crate::setup::nofollow::metadata_nofollow(&root.file, Path::new(COMMIT))?,
        ) {
            return Err(invalid());
        }
        durability::sync_file(&source)?;
        durability::sync_file(&marker)?;
        durability::sync_directory(&directory.file)?;
        durability::sync_directory(&root.file)?;
        package.validate_completion_origin(verified)?;
        verified.commit_completed_setup(&committed.setup, Sha256::digest(&bytes).into())?;
        check_cancel(cancel)?;
        Ok(Some(CompletedSetupReceipt {
            setup: committed.setup,
        }))
    })
}
