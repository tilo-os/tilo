//! Locate only the fixed protected pending inbox on the held selected boot medium.
use super::protected_output::PendingArea;
use super::*;
use crate::setup::prepared_setup::{LIMITS, PreparedSetupReview};
use cap_fs_ext::{OpenOptionsExt as _, OpenOptionsSyncExt as _};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
};
use tilo_files::native::atomic::{Privacy, StorageDirectory};

/// Owns the independent authenticated quarantine, never an inbox path grant.
pub struct AuthenticatedPendingSetup {
    _quarantine: tempfile::TempDir,
    setup: Arc<PreparedSetupReview>,
    expectation: crate::imager_model::PreparedSetupExpectation,
}
impl AuthenticatedPendingSetup {
    pub fn authenticated_setup(&self) -> Arc<PreparedSetupReview> {
        self.setup.clone()
    }
    pub fn pending_expectation(&self) -> &crate::imager_model::PreparedSetupExpectation {
        &self.expectation
    }
    pub fn validate(&self) -> io::Result<()> {
        self.setup.validate()
    }
}
pub struct PendingBootReview<'a> {
    medium: &'a super::boot_medium::BootMedium,
    capsule: Arc<AuthenticatedPendingSetup>,
    binding: crate::media_layout::MediaPartitionBinding,
    private: Arc<PinnedDirectory>,
    inbox: File,
    package_directory: File,
    source: File,
    source_snapshot: cap_primitives::fs::Metadata,
    inbox_snapshot: cap_primitives::fs::Metadata,
    record_snapshot: cap_primitives::fs::Metadata,
    area: PendingArea,
}
impl<'a> PendingBootReview<'a> {
    pub fn review(
        medium: &'a super::boot_medium::BootMedium,
        password: &[u8],
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        medium.with_root(|root, binding| review_pending(medium, root, binding, password, cancel))
    }
    pub fn authenticated_setup(&self) -> Arc<PreparedSetupReview> {
        self.capsule.authenticated_setup()
    }
    pub fn pending_expectation(&self) -> &crate::imager_model::PreparedSetupExpectation {
        self.capsule.pending_expectation()
    }
    pub fn retained_setup(&self) -> io::Result<Arc<AuthenticatedPendingSetup>> {
        self.validate()?;
        let retained = self.capsule.clone();
        self.validate()?;
        Ok(retained)
    }
    /// Keep payload descriptors strictly inside the genuine readonly boot scope.
    pub fn with_payload<T>(&self, callback: impl FnOnce(&File) -> io::Result<T>) -> io::Result<T> {
        self.validate()?;
        let result = callback(&self.inbox);
        self.validate()?;
        result
    }
    pub fn selected_binding(&self) -> &crate::media_layout::MediaPartitionBinding {
        &self.binding
    }
    pub fn validate(&self) -> io::Result<()> {
        self.capsule.validate()?;
        self.medium.with_root(|root, binding| {
            if binding != &self.binding {
                return Err(changed("Pending boot medium changed"));
            }
            root.validate_destination()?;
            self.private.validate_destination()?;
            let storage =
                StorageDirectory::from_file(durability::readable_directory(&self.private.file)?)?;
            let mut file = storage.open_read(
                std::ffi::OsStr::new("original-pending-area.json"),
                Privacy::OwnerOnly,
            )?;
            let mut bytes = Vec::new();
            std::io::Read::by_ref(&mut file)
                .take(4097)
                .read_to_end(&mut bytes)?;
            if PendingArea::parse(&bytes)? != self.area
                || !crate::setup::migration::same_snapshot(
                    &self.record_snapshot,
                    &cap_primitives::fs::Metadata::from_file(&file)?,
                )
                || !crate::setup::migration::same_snapshot(
                    &self.inbox_snapshot,
                    &cap_primitives::fs::Metadata::from_file(&self.inbox)?,
                )
                || !crate::setup::migration::same_snapshot(
                    &self.inbox_snapshot,
                    &crate::setup::nofollow::metadata_nofollow(
                        &root.file,
                        Path::new(&self.area.name),
                    )?,
                )
                || !crate::setup::migration::same_snapshot(
                    &self.source_snapshot,
                    &cap_primitives::fs::Metadata::from_file(&self.source)?,
                )
                || !crate::setup::migration::same_snapshot(
                    &self.source_snapshot,
                    &crate::setup::nofollow::metadata_nofollow(
                        &self.package_directory,
                        Path::new(FILE),
                    )?,
                )
            {
                return Err(changed("Pending boot custody changed"));
            }
            Ok(())
        })
    }
}
fn open_directory(parent: &File, name: &str) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .custom_flags(libc::O_DIRECTORY | libc::O_NONBLOCK);
    cap_primitives::fs::open(parent, Path::new(name), &options)
}
fn review_pending<'a>(
    medium: &'a super::boot_medium::BootMedium,
    root: &Arc<PinnedDirectory>,
    binding: &crate::media_layout::MediaPartitionBinding,
    password: &[u8],
    cancel: &AtomicBool,
) -> io::Result<PendingBootReview<'a>> {
    // SAFETY: geteuid only observes the executing root controller's credentials.
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(changed("Boot pending setup review requires root custody"));
    }
    check_cancel(cancel)?;
    root.validate_destination()?;
    let private_metadata =
        crate::setup::nofollow::metadata_nofollow(&root.file, Path::new(DIRECTORY))?;
    let private = root.open_child(std::ffi::OsStr::new(DIRECTORY), &private_metadata)?;
    private.validate_destination()?;
    let storage = StorageDirectory::from_file(durability::readable_directory(&private.file)?)?;
    storage.validate_privacy(Privacy::OwnerOnly)?;
    let mut record = storage.open_read(
        std::ffi::OsStr::new("original-pending-area.json"),
        Privacy::OwnerOnly,
    )?;
    let record_before = cap_primitives::fs::Metadata::from_file(&record)?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut record)
        .take(4097)
        .read_to_end(&mut bytes)?;
    let area = PendingArea::parse(&bytes)?;
    if area.medium != *binding
        || area.setup.sealed_bytes
            > LIMITS
                .sealed_bound()
                .ok_or_else(|| changed("Setup bound overflow"))?
    {
        return Err(changed("Pending setup differs from selected boot medium"));
    }
    let inbox = open_directory(&root.file, &area.name)?;
    let inbox_before = cap_primitives::fs::Metadata::from_file(&inbox)?;
    let actual = inbox.metadata()?;
    if actual.uid() != area.original_uid
        || actual.gid() != 0
        || actual.mode() & 0o7777 != 0o700
        || actual.ino() != area.inode
        || actual.dev() != root.file.metadata()?.dev()
    {
        return Err(changed("Protected boot inbox identity differs"));
    }
    let directory = open_directory(&inbox, DIRECTORY)?;
    let actual = directory.metadata()?;
    if actual.uid() != area.original_uid
        || actual.mode() & 0o022 != 0
        || actual.dev() != inbox.metadata()?.dev()
    {
        return Err(changed("Pending setup directory custody differs"));
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut source = cap_primitives::fs::open(&directory, Path::new(FILE), &options)?;
    let before = cap_primitives::fs::Metadata::from_file(&source)?;
    if !before.is_file()
        || before.uid() != area.original_uid
        || before.mode() & 0o022 != 0
        || before.len() != area.setup.sealed_bytes
    {
        return Err(changed("Pending setup ciphertext custody differs"));
    }
    let quarantine = tempfile::Builder::new()
        .prefix("tilo-boot-pending-setup-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let package = quarantine.path().join(FILE);
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&package)?;
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        check_cancel(cancel)?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .filter(|n| *n <= area.setup.sealed_bytes)
            .ok_or_else(|| changed("Pending setup ciphertext exceeds bound"))?;
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    output.sync_all()?;
    if total != area.setup.sealed_bytes
        || hex::encode(hash.finalize()) != area.setup.ciphertext_sha256
        || !crate::setup::migration::same_snapshot(
            &before,
            &cap_primitives::fs::Metadata::from_file(&source)?,
        )
        || !crate::setup::migration::same_snapshot(
            &before,
            &crate::setup::nofollow::metadata_nofollow(&directory, Path::new(FILE))?,
        )
    {
        return Err(changed(
            "Pending setup ciphertext changed during authentication",
        ));
    }
    let setup =
        PreparedSetupReview::open(&package, password, &quarantine.path().join("setup"), cancel)?;
    let plan = setup.pending_original_declaration()?;
    let plans_digest: [u8; 32] =
        Sha256::digest(serde_json::to_vec(&plan.authenticated_source_plans()?)?).into();
    if plan.media() != binding
        || plan.source().operation_id != area.operation
        || plan.requested_caller().uid() != area.original_uid
        || plans_digest != area.plans_sha256
    {
        return Err(changed(
            "Authenticated pending setup differs from protected inbox receipt",
        ));
    }
    root.validate_destination()?;
    if !crate::setup::migration::same_snapshot(
        &inbox_before,
        &cap_primitives::fs::Metadata::from_file(&inbox)?,
    ) || !crate::setup::migration::same_snapshot(
        &inbox_before,
        &crate::setup::nofollow::metadata_nofollow(&root.file, Path::new(&area.name))?,
    ) || !crate::setup::migration::same_snapshot(
        &record_before,
        &cap_primitives::fs::Metadata::from_file(&record)?,
    ) || !crate::setup::migration::same_snapshot(
        &record_before,
        &crate::setup::nofollow::metadata_nofollow(
            &private.file,
            Path::new("original-pending-area.json"),
        )?,
    ) {
        return Err(changed("Protected pending inbox changed during review"));
    }
    check_cancel(cancel)?;
    Ok(PendingBootReview {
        medium,
        capsule: Arc::new(AuthenticatedPendingSetup {
            _quarantine: quarantine,
            setup: Arc::new(setup),
            expectation: area.setup.clone(),
        }),
        binding: binding.clone(),
        private,
        inbox,
        package_directory: directory,
        source,
        source_snapshot: before,
        inbox_snapshot: inbox_before,
        record_snapshot: record_before,
        area,
    })
}
