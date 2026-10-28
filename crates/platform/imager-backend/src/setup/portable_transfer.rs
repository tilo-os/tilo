//! Password-sealed portable migration. Export never stages a plaintext home.
//! Imported trees remain quarantined until the caller explicitly assigns them.
use super::{
    migration::{PinnedDirectory, changed, check_cancel},
    migration_durability as durability,
    migration_review::SourceInventory,
};
use sha2::Digest as _;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
use zeroize::Zeroizing;
pub mod acl;
pub mod archive;
pub(crate) mod codec;
mod process;
mod progress;
pub use progress::{ArchivePhase, ArchiveProgress, Observer};
mod source_sizes;
mod sparse;
use source_sizes::source_sizes;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Home,
    PreparedSetup,
    PreparationAccounts,
    FlatpakApplications,
    BrowserProfiles,
}
/// Bounds must come from the explicitly allocated medium/storage, not an estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub plaintext_bytes: u64,
    pub entries: usize,
    pub metadata_bytes: usize,
}
impl Limits {
    fn validate(self) -> io::Result<()> {
        if self.plaintext_bytes == 0
            || self.entries == 0
            || self.entries > 1_000_000
            || self.metadata_bytes == 0
            || self.metadata_bytes > 512 * 1024 * 1024
        {
            return Err(changed("Portable transfer review bounds are invalid"));
        }
        self.sealed_bound()
            .ok_or_else(|| changed("Portable transfer allocation overflows"))?;
        Ok(())
    }
    pub fn sealed_bound(self) -> Option<u64> {
        self.plaintext_bytes.checked_add(2048)?.checked_add(
            self.plaintext_bytes
                .div_ceil(codec::CHUNK as u64)
                .checked_mul(256)?,
        )
    }
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExportSummary {
    pub files: u64,
    pub bytes: u64,
    pub links: u64,
    pub special_files: u64,
    pub runtime_sockets: u64,
}
pub struct ExportReview {
    source: Arc<PinnedDirectory>,
    inventory: SourceInventory,
    package: PathBuf,
    purpose: Purpose,
    limits: Limits,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct PackageInfo {
    pub purpose: Purpose,
    /// Total regular-file logical length; sparse holes consume no archive payload.
    pub logical_bytes: u64,
    pub plaintext_bytes: u64,
    pub sealed_bytes: u64,
    pub entries: usize,
    pub runtime_sockets: u64,
    pub setup_binding: Option<String>,
}
impl ExportReview {
    pub fn review(
        source: &Path,
        package: &Path,
        purpose: Purpose,
        limits: Limits,
    ) -> io::Result<Self> {
        limits.validate()?;
        let source_pin = PinnedDirectory::open_absolute(source)?;
        let parent = PinnedDirectory::open_absolute(
            package
                .parent()
                .ok_or_else(|| changed("Package requires a parent directory"))?,
        )?;
        parent.validate_destination()?;
        if parent.contains_identity(&source_pin.identity) || package.exists() {
            return Err(changed("Package must be new and outside the source tree"));
        }
        let inventory = SourceInventory::capture_home(&source_pin)?;
        let summary = inventory.summary()?;
        let data_bytes = if matches!(purpose, Purpose::Home | Purpose::FlatpakApplications) {
            stored_bytes(&source_pin, &inventory)?
        } else {
            summary.bytes
        };
        if summary.special_files != 0 || data_bytes > limits.plaintext_bytes {
            return Err(changed(
                "Portable transfer cannot include special files or exceed allocated storage",
            ));
        }
        Ok(Self {
            source: source_pin,
            inventory,
            package: package.to_owned(),
            purpose,
            limits,
        })
    }
    pub fn summary(&self) -> io::Result<ExportSummary> {
        let summary = self.inventory.summary()?;
        Ok(ExportSummary {
            files: summary.files,
            bytes: summary.bytes,
            links: summary.links,
            special_files: summary.special_files,
            runtime_sockets: summary.runtime_sockets,
        })
    }
    pub fn export(
        &self,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<PackageInfo> {
        self.export_inner(password, cancel, None, &mut |_| Ok(()))
    }
    /// Bind a separately purposed package to its authenticated prepared setup.
    /// # Errors
    /// Refuses empty bindings, changed input or incomplete encryption.
    pub fn export_bound(
        &self,
        password: Zeroizing<Vec<u8>>,
        binding: &str,
        cancel: &AtomicBool,
    ) -> io::Result<PackageInfo> {
        if binding.is_empty() || binding.len() > 256 || binding.chars().any(char::is_control) {
            return Err(changed("Portable package requires a valid setup binding"));
        }
        self.export_inner(password, cancel, Some(binding), &mut |_| Ok(()))
    }
    pub fn export_bound_reporting(
        &self,
        password: Zeroizing<Vec<u8>>,
        binding: &str,
        cancel: &AtomicBool,
        observer: &mut Observer<'_>,
    ) -> io::Result<PackageInfo> {
        if binding.is_empty() || binding.len() > 256 || binding.chars().any(char::is_control) {
            return Err(changed("Portable package requires a valid setup binding"));
        }
        self.export_inner(password, cancel, Some(binding), observer)
    }
    pub fn export_reporting(
        &self,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
        observer: &mut Observer<'_>,
    ) -> io::Result<PackageInfo> {
        self.export_inner(password, cancel, None, observer)
    }
    fn export_inner(
        &self,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
        binding: Option<&str>,
        observer: &mut Observer<'_>,
    ) -> io::Result<PackageInfo> {
        use cap_fs_ext::OpenOptionsFollowExt;
        use cap_primitives::fs::{FollowSymlinks, OpenOptions};
        self.source.validate()?;
        self.inventory.validate(&self.source)?;
        check_cancel(cancel)?;
        let parent = PinnedDirectory::open_absolute(
            self.package
                .parent()
                .ok_or_else(|| changed("Package parent missing"))?,
        )?;
        parent.validate_destination()?;
        if parent.contains_identity(&self.source.identity) {
            return Err(changed("Package parent entered source tree"));
        }
        let name = self
            .package
            .file_name()
            .ok_or_else(|| changed("Package name missing"))?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        use cap_primitives::fs::OpenOptionsExt;
        options.mode(0o600);
        let mut file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
        let result = export_stream_reporting(
            &self.source,
            &self.inventory,
            self.purpose,
            self.limits,
            &mut file,
            &mut ExportWork {
                password: &password,
                cancel,
                setup_binding: binding,
                observer,
            },
        )
        .and_then(|info| {
            parent.validate()?;
            durability::sync_directory(&parent.file)?;
            Ok(info)
        });
        if result.is_err() {
            let _ = cap_primitives::fs::remove_file(&parent.file, Path::new(name));
        }
        result
    }
}

/// Shared canonical exporter; callers hold the destination inode and grant.
pub struct ExportWork<'a, 'b> {
    pub password: &'a [u8],
    pub cancel: &'a AtomicBool,
    pub setup_binding: Option<&'a str>,
    pub observer: &'a mut Observer<'b>,
}
pub fn export_stream_reporting(
    source: &Arc<PinnedDirectory>,
    inventory: &SourceInventory,
    purpose: Purpose,
    limits: Limits,
    file: &mut File,
    work: &mut ExportWork<'_, '_>,
) -> io::Result<PackageInfo> {
    let password = work.password;
    let cancel = work.cancel;
    let setup_binding = work.setup_binding;
    limits.validate()?;
    source.validate()?;
    inventory.validate(source)?;
    check_cancel(cancel)?;
    let mut sealer = codec::Sealer::new(
        &mut *file,
        password,
        purpose,
        limits.plaintext_bytes,
        cancel,
    )?;
    let sparse_encoding = matches!(purpose, Purpose::Home | Purpose::FlatpakApplications);
    let sparse_candidate = if sparse_encoding {
        source_sizes(source, inventory, cancel)?.1
    } else {
        false
    };
    process::create(
        source,
        inventory,
        &mut progress::Stream::new(&mut sealer, ArchivePhase::Archiving, work.observer)?,
        cancel,
        sparse_encoding,
    )?;
    source.validate()?;
    inventory.validate(source)?;
    let summary = inventory.summary()?;
    let (_, footer) = sealer.finish_bound(
        summary.runtime_sockets,
        setup_binding,
        sparse_candidate.then_some(summary.bytes),
    )?;
    durability::sync_file(file)?;
    file.seek(SeekFrom::Start(0))?;
    let (verified, _) = archive::scan_reporting(
        codec::Opener::new(&mut *file, password, limits.plaintext_bytes, cancel)?,
        limits,
        work.observer,
    )?;
    (work.observer)(ArchiveProgress {
        phase: ArchivePhase::VerifyingSource,
        completed: 0,
        total: None,
    })?;
    archive::verify_source_tree(source, &verified, inventory, cancel)?;
    inventory.validate(source)?;
    Ok(PackageInfo {
        purpose,
        logical_bytes: archive::logical_bytes(&verified)?,
        plaintext_bytes: footer.bytes,
        sealed_bytes: file.metadata()?.len(),
        entries: verified.len(),
        runtime_sockets: footer.runtime_sockets,
        setup_binding: footer.setup_binding,
    })
}

pub fn verify_export_stream_reporting(
    source: &Arc<PinnedDirectory>,
    inventory: &SourceInventory,
    file: &mut File,
    password: &[u8],
    limits: Limits,
    cancel: &AtomicBool,
    observer: &mut Observer<'_>,
) -> io::Result<PackageInfo> {
    limits.validate()?;
    source.validate()?;
    inventory.validate(source)?;
    file.seek(SeekFrom::Start(0))?;
    let opener = codec::Opener::new(&mut *file, password, limits.plaintext_bytes, cancel)?;
    let purpose = opener.purpose;
    let (verified, footer) = archive::scan_reporting(opener, limits, observer)?;
    observer(ArchiveProgress {
        phase: ArchivePhase::VerifyingSource,
        completed: 0,
        total: None,
    })?;
    archive::verify_source_tree(source, &verified, inventory, cancel)?;
    inventory.validate(source)?;
    Ok(PackageInfo {
        purpose,
        logical_bytes: archive::logical_bytes(&verified)?,
        plaintext_bytes: footer.bytes,
        sealed_bytes: file.metadata()?.len(),
        entries: verified.len(),
        runtime_sockets: footer.runtime_sockets,
        setup_binding: footer.setup_binding,
    })
}

/// Size of data extents emitted by fixed GNU seek-hole sparse encoding.
/// Inodes are deduplicated exactly as GNU tar's hardlink handling; source
/// metadata is checked before/after every held member open.
pub fn stored_bytes(root: &Arc<PinnedDirectory>, inventory: &SourceInventory) -> io::Result<u64> {
    source_sizes(root, inventory, &AtomicBool::new(false)).map(|(stored, _)| stored)
}
/// Check the exact reviewed Linux ACL policy before presenting Home as ready.
/// Metadata-only admission: no ciphertext, file-content reads or source writes.
#[cfg(target_os = "linux")]
pub fn validate_home_metadata(
    root: &Arc<PinnedDirectory>,
    inventory: &SourceInventory,
    cancel: &AtomicBool,
) -> io::Result<()> {
    fn walk(
        directory: &Arc<PinnedDirectory>,
        relative: &Path,
        inventory: &SourceInventory,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
        use cap_primitives::fs::{FollowSymlinks, OpenOptions};
        use cap_std::fs::Dir;
        check_cancel(cancel)?;
        let readable = durability::readable_directory(&directory.file)?;
        acl::policy(&readable)?;
        let entries = Dir::from_std_file(readable);
        for entry in entries.entries()? {
            check_cancel(cancel)?;
            let name = entry?.file_name();
            let path = relative.join(&name);
            if inventory.excluded(&path)? {
                continue;
            }
            let metadata = super::nofollow::metadata_nofollow(&directory.file, Path::new(&name))?;
            inventory.check(&path, &metadata)?;
            if metadata.is_dir() && !metadata.is_symlink() {
                walk(
                    &directory.open_child(&name, &metadata)?,
                    &path,
                    inventory,
                    cancel,
                )?;
            } else if metadata.is_file() {
                let mut options = OpenOptions::new();
                options.read(true).follow(FollowSymlinks::No).nonblock(true);
                let file = cap_primitives::fs::open(&directory.file, Path::new(&name), &options)?;
                inventory.check(&path, &cap_primitives::fs::Metadata::from_file(&file)?)?;
                acl::policy(&file)?;
                inventory.check(&path, &cap_primitives::fs::Metadata::from_file(&file)?)?;
            }
        }
        directory.validate()
    }
    inventory.validate(root)?;
    walk(root, Path::new(""), inventory, cancel)?;
    inventory.validate(root)
}

pub struct ImportReview {
    file: File,
    info: PackageInfo,
    entries: archive::Records,
    limits: Limits,
    ciphertext: [u8; 32],
}
pub struct VerifiedTree {
    pub purpose: Purpose,
    pub root: PathBuf,
    pub info: PackageInfo,
}
impl ImportReview {
    pub fn review(
        package: &Path,
        password: &[u8],
        purpose: Purpose,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        Self::review_in(
            package,
            password,
            purpose,
            limits,
            cancel,
            archive::Scratch::default(),
        )
    }
    #[cfg(target_os = "linux")]
    pub fn working_storage(&self) -> Option<Arc<super::migration_review::InventoryScratch>> {
        self.entries.working_storage()
    }
    #[cfg(target_os = "linux")]
    pub fn release_verified_restoration(&self) -> io::Result<()> {
        if let Some(scratch) = self.entries.working_storage() {
            scratch.release_verified_restoration()?;
        }
        Ok(())
    }
    /// Retain the independently approved scratch allocation through review,
    /// extraction, ACL restoration and every subsequent verification pass.
    pub fn review_in(
        package: &Path,
        password: &[u8],
        purpose: Purpose,
        limits: Limits,
        cancel: &AtomicBool,
        scratch: archive::Scratch,
    ) -> io::Result<Self> {
        limits.validate()?;
        use cap_fs_ext::OpenOptionsFollowExt;
        use cap_primitives::fs::{FollowSymlinks, OpenOptions};
        let parent = PinnedDirectory::open_absolute(
            package
                .parent()
                .ok_or_else(|| changed("Package parent missing"))?,
        )?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = cap_primitives::fs::open(
            &parent.file,
            Path::new(
                package
                    .file_name()
                    .ok_or_else(|| changed("Package name missing"))?,
            ),
            &options,
        )?;
        parent.validate()?;
        Self::review_file_in(file, password, purpose, limits, cancel, scratch)
    }
    /// Authenticate the full stream from an already received descriptor. Paths,
    /// receipts and sender completion flags do not substitute for this scan.
    pub fn review_file_in(
        mut file: File,
        password: &[u8],
        purpose: Purpose,
        limits: Limits,
        cancel: &AtomicBool,
        scratch: archive::Scratch,
    ) -> io::Result<Self> {
        limits.validate()?;
        if !file.metadata()?.is_file()
            || file.metadata()?.len()
                > limits
                    .sealed_bound()
                    .ok_or_else(|| changed("Invalid package size bound"))?
        {
            return Err(changed("Package must be a regular file"));
        }
        let before_hash = hash_file(&mut file, cancel)?;
        let opener = codec::Opener::new(&mut file, password, limits.plaintext_bytes, cancel)?;
        if opener.purpose != purpose {
            return Err(changed(
                "Migration package purpose does not match this operation",
            ));
        }
        let (entries, footer) = archive::scan_in(opener, limits, &mut |_| Ok(()), scratch)?;
        if footer.runtime_sockets > limits.entries as u64 {
            return Err(changed(
                "Runtime socket disclosure exceeds reviewed entry bounds",
            ));
        }
        let sealed_bytes = file.metadata()?.len();
        let ciphertext = hash_file(&mut file, cancel)?;
        if ciphertext != before_hash {
            return Err(changed("Migration package changed during review"));
        }
        Ok(Self {
            file,
            info: PackageInfo {
                purpose,
                logical_bytes: archive::logical_bytes(&entries)?,
                plaintext_bytes: footer.bytes,
                sealed_bytes,
                entries: entries.len(),
                runtime_sockets: footer.runtime_sockets,
                setup_binding: footer.setup_binding,
            },
            entries,
            limits,
            ciphertext,
        })
    }
    pub fn info(&self) -> &PackageInfo {
        &self.info
    }
    /// Digest authenticated during review. Delivery revalidates the held bytes.
    pub fn ciphertext_digest(&self) -> [u8; 32] {
        self.ciphertext
    }
    /// Revalidate the held ciphertext before accepting an existing delivery.
    pub fn verify_held_ciphertext(&mut self, cancel: &AtomicBool) -> io::Result<()> {
        if hash_file(&mut self.file, cancel)? != self.ciphertext {
            return Err(changed("Reviewed migration package changed"));
        }
        Ok(())
    }
    /// Copy only the already authenticated ciphertext to a caller-owned output.
    /// The output is never plaintext and the source descriptor stays pinned.
    pub fn copy_ciphertext(
        &mut self,
        output: &mut File,
        cancel: &AtomicBool,
    ) -> io::Result<[u8; 32]> {
        self.copy_ciphertext_reporting(output, cancel, &mut |_| Ok(()))
    }
    pub fn copy_ciphertext_reporting(
        &mut self,
        output: &mut File,
        cancel: &AtomicBool,
        observer: &mut Observer<'_>,
    ) -> io::Result<[u8; 32]> {
        if crate::imager_model::SourceIdentity::from_file(&self.file)?
            == crate::imager_model::SourceIdentity::from_file(output)?
        {
            return Err(changed("Ciphertext delivery must not overwrite its source"));
        }
        if hash_file(&mut self.file, cancel)? != self.ciphertext {
            return Err(changed("Reviewed migration package changed"));
        }
        self.file.seek(SeekFrom::Start(0))?;
        observer(ArchiveProgress {
            phase: ArchivePhase::DeliveringCiphertext,
            completed: 0,
            total: Some(self.info.sealed_bytes),
        })?;
        let mut buffer = [0; 65536];
        let mut written = 0u64;
        loop {
            check_cancel(cancel)?;
            let count = self.file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            written = written
                .checked_add(count as u64)
                .filter(|size| *size <= self.info.sealed_bytes)
                .ok_or_else(|| changed("Reviewed ciphertext size changed"))?;
            output.write_all(&buffer[..count])?;
            observer(ArchiveProgress {
                phase: ArchivePhase::DeliveringCiphertext,
                completed: written,
                total: Some(self.info.sealed_bytes),
            })?;
        }
        if written != self.info.sealed_bytes
            || hash_file(&mut self.file, cancel)? != self.ciphertext
        {
            return Err(changed(
                "Reviewed migration package changed during delivery",
            ));
        }
        output.sync_all()?;
        if hash_file(output, cancel)? != self.ciphertext {
            return Err(changed("Delivered ciphertext read-back differs"));
        }
        Ok(self.ciphertext)
    }
    /// `quarantine` must name a new private container, never a live home or overlay.
    pub fn verify_existing_tree(&mut self, root: &Path, cancel: &AtomicBool) -> io::Result<()> {
        self.verify_held_ciphertext(cancel)?;
        let pin = PinnedDirectory::open_absolute(root)?;
        super::destination_security::check_private_file(&pin.file)?;
        self.entries.check_existing_root(&pin)?;
        archive::verify_tree(&pin, &self.entries, true, cancel)?;
        self.entries.check_existing_root(&pin)?;
        self.verify_held_ciphertext(cancel)
    }
    pub fn verify_adoption(
        &mut self,
        witness: &super::home_transfer::promotion::AdoptionWitness,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.verify_held_ciphertext(cancel)?;
        self.entries.check_existing_root(&witness.quarantine)?;
        archive::verify_mapped(&self.entries, witness, cancel)?;
        self.entries.check_existing_root(&witness.quarantine)?;
        self.verify_held_ciphertext(cancel)
    }
    pub fn verify_adoption_after_applications(
        &mut self,
        witness: &super::home_transfer::promotion::AdoptionWitness,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.verify_held_ciphertext(cancel)?;
        self.entries.check_existing_root(&witness.quarantine)?;
        archive::verify_mapped_after_applications(&self.entries, witness, cancel)?;
        self.entries.check_existing_root(&witness.quarantine)?;
        self.verify_held_ciphertext(cancel)
    }
    pub fn verify_original_application_config(&self, bytes: &[u8]) -> io::Result<()> {
        let record = self
            .entries
            .get(Path::new(".local/share/flatpak/repo/config"))?;
        match record {
            None if bytes.is_empty() => Ok(()),
            Some(record)
                if matches!(&record.kind, archive::Kind::File(hash)
                if record.size == u64::try_from(bytes.len()).unwrap_or(u64::MAX)
                    && *hash == <[u8;32]>::from(sha2::Sha256::digest(bytes))) =>
            {
                Ok(())
            }
            _ => Err(changed(
                "Original application configuration differs from authenticated archive",
            )),
        }
    }
    pub fn extract(
        mut self,
        password: Zeroizing<Vec<u8>>,
        quarantine: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<VerifiedTree> {
        self.extract_held(password, quarantine, cancel)
    }
    pub fn extract_held(
        &mut self,
        password: Zeroizing<Vec<u8>>,
        quarantine: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<VerifiedTree> {
        check_cancel(cancel)?;
        self.entries.require_restoration()?;
        if hash_file(&mut self.file, cancel)? != self.ciphertext {
            return Err(changed("Reviewed migration package changed"));
        }
        let parent = PinnedDirectory::open_absolute(
            quarantine
                .parent()
                .ok_or_else(|| changed("Quarantine parent missing"))?,
        )?;
        parent.validate_destination()?;
        let name = quarantine
            .file_name()
            .ok_or_else(|| changed("Quarantine name missing"))?;
        // Only a fully authenticated and verified container gets the requested
        // name. An interrupted attempt cannot masquerade as completed custody
        // or prevent a later attempt from using the same quarantine path.
        match super::nofollow::metadata_nofollow(&parent.file, Path::new(name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => return Err(changed("Quarantine already exists; it was not replaced")),
        }
        self.entries.check_restoration()?;
        let attempt_name = format!(".tilo-import-{}", uuid::Uuid::new_v4());
        let container = parent.create_child(std::ffi::OsStr::new(&attempt_name))?;
        let home = container.create_child(std::ffi::OsStr::new("home"))?;
        self.file.seek(SeekFrom::Start(0))?;
        let mut opener = codec::Opener::new(
            &mut self.file,
            &password,
            self.limits.plaintext_bytes,
            cancel,
        )?;
        if opener.purpose != self.info.purpose {
            return Err(changed("Migration package purpose changed"));
        }
        process::extract(&home, &mut opener, cancel)?;
        if opener.footer.is_none() || hash_file(&mut self.file, cancel)? != self.ciphertext {
            return Err(changed("Migration package was not fully authenticated"));
        }
        use std::os::unix::fs::PermissionsExt;
        durability::readable_directory(&home.file)?
            .set_permissions(std::fs::Permissions::from_mode(0o700))?;
        archive::restore_authenticated_acls(&home, &self.entries, cancel)?;
        archive::verify_tree(&home, &self.entries, true, cancel)?;
        home.validate()?;
        container.validate()?;
        durability::sync_directory(&home.file)?;
        durability::sync_directory(&container.file)?;
        publish_import(&parent, &container, &attempt_name, name, cancel)?;
        Ok(VerifiedTree {
            purpose: self.info.purpose,
            root: quarantine.join("home"),
            info: self.info.clone(),
        })
    }
}
/// Publish only this held, verified private attempt. Existing names are never
/// overwritten. Failed attempts remain private siblings for explicit cleanup.
fn publish_import(
    parent: &Arc<PinnedDirectory>,
    container: &Arc<PinnedDirectory>,
    attempt: &str,
    name: &std::ffi::OsStr,
    cancel: &AtomicBool,
) -> io::Result<()> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    check_cancel(cancel)?;
    container.validate_destination()?;
    parent.validate_destination()?;
    let attempt = std::ffi::CString::new(attempt).map_err(io::Error::other)?;
    let name_c = std::ffi::CString::new(name.as_bytes()).map_err(io::Error::other)?;
    // SAFETY: both names are single components and terminated; the held parent
    // remains live. NOREPLACE refuses a concurrently appearing destination.
    let result = unsafe {
        libc::renameat2(
            parent.file.as_raw_fd(),
            attempt.as_ptr(),
            parent.file.as_raw_fd(),
            name_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    durability::sync_directory(&parent.file)?;
    let published = super::nofollow::metadata_nofollow(&parent.file, Path::new(name))?;
    if !published.is_dir()
        || !super::nofollow::same_directory_identity(&published, &container.identity)
    {
        return Err(changed("Published quarantine identity changed"));
    }
    parent.validate_destination()
}

fn hash_file(file: &mut File, cancel: &AtomicBool) -> io::Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(hash.finalize().into())
}
