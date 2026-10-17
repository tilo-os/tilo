//! Separately consented, bounded browser data carried without a home grant.
use crate::browser_selection::SelectedSources;
use browser_continuity::{PLAN_FILE, PROFILES_DIRECTORY, Plan};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io, os::unix::fs::PermissionsExt, path::Path, sync::atomic::AtomicBool};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
use zeroize::Zeroizing;

use super::{
    migration::{PinnedDirectory, changed, check_cancel},
    portable_transfer::{ExportReview, ImportReview, Limits, Purpose, VerifiedTree},
};

pub const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = MAX_BYTES + 1024 * 1024;
const MAX_ENTRIES: usize = 128;
const MAX_METADATA: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    pub ciphertext_sha256: [u8; 32],
    pub plan_sha256: [u8; 32],
    pub limits: Limits,
}
impl Expectation {
    /// # Errors
    /// Refuses malformed declarations or payloads beyond the fixed browser bounds.
    pub fn validate(&self) -> io::Result<()> {
        let operation = uuid::Uuid::parse_str(&self.operation).map_err(io::Error::other)?;
        if self.schema != 1
            || operation.to_string() != self.operation
            || self.purpose != Purpose::BrowserProfiles
            || self.filename != format!("browser-{}.sealed", self.operation)
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.setup_binding.chars().any(char::is_control)
            || self.sealed_bytes == 0
            || self.plaintext_bytes == 0
            || self.plaintext_bytes > self.limits.plaintext_bytes
            || self.limits.plaintext_bytes > MAX_ARCHIVE_BYTES
            || self.entries == 0
            || self.entries > self.limits.entries
            || self.limits.entries > MAX_ENTRIES
            || self.limits.metadata_bytes == 0
            || self.limits.metadata_bytes > MAX_METADATA
            || self
                .limits
                .sealed_bound()
                .is_none_or(|bound| self.sealed_bytes > bound)
        {
            return Err(changed("Invalid browser transfer expectation"));
        }
        Ok(())
    }
    /// # Errors
    /// Refuses a different setup, identity, selection or authenticated plan digest.
    pub fn validate_plan(&self, plan: &Plan) -> io::Result<()> {
        self.validate()?;
        let bytes = plan.encode().map_err(io::Error::other)?;
        if plan.profiles.is_empty()
            || plan.setup_binding != self.setup_binding
            || digest(&bytes) != self.plan_sha256
        {
            return Err(changed(
                "Browser transfer differs from its authenticated plan",
            ));
        }
        Ok(())
    }
}

pub struct ExportRequest<'a> {
    pub staging_parent: &'a Path,
    pub package: &'a Path,
    pub operation: &'a str,
    pub password: Zeroizing<Vec<u8>>,
}

/// Staging is always private and removed before a successful result is returned.
/// Only the separately selected browser domains enter this carrier.
pub fn export(
    selected: &SelectedSources,
    plan: &Plan,
    request: ExportRequest<'_>,
    cancel: &AtomicBool,
) -> io::Result<Expectation> {
    let expected_plan = selected.plan(plan.setup_binding.clone(), plan.identity_public_key)?;
    if &expected_plan != plan || plan.profiles.is_empty() {
        return Err(changed(
            "Browser sources differ from their reviewed selection",
        ));
    }
    let operation = uuid::Uuid::parse_str(request.operation).map_err(io::Error::other)?;
    if operation.to_string() != request.operation
        || request.package.file_name().and_then(|name| name.to_str())
            != Some(format!("browser-{}.sealed", request.operation).as_str())
    {
        return Err(changed(
            "Browser transfer requires its exact operation filename",
        ));
    }
    let parent = PinnedDirectory::open_absolute(request.staging_parent)?;
    parent.validate_destination()?;
    super::destination_security::check_private_file(&parent.file)?;
    let staging = tempfile::Builder::new()
        .prefix("browser-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(request.staging_parent)?;
    let pin = PinnedDirectory::open_absolute(staging.path())?;
    pin.validate_destination()?;
    let plan_bytes = plan.encode().map_err(io::Error::other)?;
    stage(selected, &pin, &plan_bytes, cancel)?;
    let limits = Limits {
        plaintext_bytes: MAX_ARCHIVE_BYTES,
        entries: MAX_ENTRIES,
        metadata_bytes: MAX_METADATA,
    };
    let review = ExportReview::review(
        staging.path(),
        request.package,
        Purpose::BrowserProfiles,
        limits,
    )?;
    let info = review.export_bound(
        Zeroizing::new(request.password.to_vec()),
        &plan.setup_binding,
        cancel,
    )?;
    let result = (|| {
        let imported = ImportReview::review(
            request.package,
            &request.password,
            Purpose::BrowserProfiles,
            limits,
            cancel,
        )?;
        let expectation = Expectation {
            schema: 1,
            operation: request.operation.into(),
            purpose: info.purpose,
            filename: format!("browser-{}.sealed", request.operation),
            setup_binding: plan.setup_binding.clone(),
            sealed_bytes: info.sealed_bytes,
            plaintext_bytes: info.plaintext_bytes,
            entries: info.entries,
            ciphertext_sha256: imported.ciphertext_digest(),
            plan_sha256: digest(&plan_bytes),
            limits,
        };
        expectation.validate_plan(plan)?;
        for (source, _) in selected.profiles() {
            source.validate()?;
        }
        parent.validate()?;
        Ok(expectation)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(request.package);
    }
    staging.close()?;
    result
}

fn stage(
    selected: &SelectedSources,
    root: &std::sync::Arc<PinnedDirectory>,
    plan_bytes: &[u8],
    cancel: &AtomicBool,
) -> io::Result<()> {
    publish(root, PLAN_FILE, plan_bytes)?;
    let profiles = root.create_child(PROFILES_DIRECTORY.as_ref())?;
    let mut bytes = 0u64;
    let mut files = 0usize;
    for (source, profile) in selected.profiles() {
        check_cancel(cancel)?;
        let snapshot = source.capture_files_cancelled(profile, cancel)?;
        bytes = bytes
            .checked_add(snapshot.bytes())
            .filter(|bytes| *bytes <= MAX_BYTES)
            .ok_or_else(|| changed("Selected browser data exceeds its transfer bound"))?;
        files += snapshot.files().len();
        if files > 48 {
            return Err(changed("Too many selected browser files"));
        }
        let directory = profiles.create_child(profile.id.as_ref())?;
        for file in snapshot.files() {
            check_cancel(cancel)?;
            publish(&directory, file.name(), file.bytes())?;
        }
        source.validate()?;
    }
    root.validate()
}
fn publish(root: &PinnedDirectory, name: &str, bytes: &[u8]) -> io::Result<()> {
    let directory =
        StorageDirectory::from_file(super::migration_durability::readable_directory(&root.file)?)?;
    directory
        .publish(
            name.as_ref(),
            bytes,
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        )
        .map_err(io::Error::other)
}
fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Authenticate the actual carrier before delivery or extraction.
pub fn review(
    expectation: &Expectation,
    plan: &Plan,
    package: &Path,
    password: &[u8],
    cancel: &AtomicBool,
) -> io::Result<ImportReview> {
    expectation.validate_plan(plan)?;
    if package.file_name().and_then(|name| name.to_str()) != Some(expectation.filename.as_str()) {
        return Err(changed("Browser transfer package name differs"));
    }
    let reviewed = ImportReview::review(
        package,
        password,
        Purpose::BrowserProfiles,
        expectation.limits,
        cancel,
    )?;
    let info = reviewed.info();
    if reviewed.ciphertext_digest() != expectation.ciphertext_sha256
        || info.setup_binding.as_deref() != Some(expectation.setup_binding.as_str())
        || info.sealed_bytes != expectation.sealed_bytes
        || info.plaintext_bytes != expectation.plaintext_bytes
        || info.entries != expectation.entries
        || info.runtime_sockets != 0
    {
        return Err(changed(
            "Browser transfer differs from its authenticated declaration",
        ));
    }
    Ok(reviewed)
}

/// Owns the decrypted tree: a dropped or failed review removes all plaintext.
/// A caller may retain it only after its authenticated handoff is committed.
pub struct Projection {
    pub tree: VerifiedTree,
    quarantine: tempfile::TempDir,
}
impl Projection {
    pub fn retain(self) -> VerifiedTree {
        let _retained_path = self.quarantine.keep();
        self.tree
    }
}

pub fn extract(
    expectation: &Expectation,
    plan: &Plan,
    package: &Path,
    password: Zeroizing<Vec<u8>>,
    staging_parent: &Path,
    cancel: &AtomicBool,
) -> io::Result<Projection> {
    let reviewed = review(expectation, plan, package, &password, cancel)?;
    let parent = PinnedDirectory::open_absolute(staging_parent)?;
    parent.validate_destination()?;
    super::destination_security::check_private_file(&parent.file)?;
    let quarantine = tempfile::Builder::new()
        .prefix("browser-import-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(staging_parent)?;
    let tree = reviewed.extract(password, &quarantine.path().join("import"), cancel)?;
    verify_tree(&tree.root, plan, cancel)?;
    parent.validate()?;
    Ok(Projection { tree, quarantine })
}

fn exact_children(directory: &PinnedDirectory, allowed: &[&str]) -> io::Result<()> {
    let directory = cap_std::fs::Dir::from_std_file(
        super::migration_durability::readable_directory(&directory.file)?,
    );
    for entry in directory.entries()? {
        let name = entry?.file_name();
        if !name.to_str().is_some_and(|name| allowed.contains(&name)) {
            return Err(changed("Browser projection contains unselected data"));
        }
    }
    Ok(())
}
/// Validate the complete projection, including absence of cookies/SHM/extra profiles.
pub fn verify_tree(root: &Path, plan: &Plan, cancel: &AtomicBool) -> io::Result<()> {
    plan.validate().map_err(io::Error::other)?;
    let root = PinnedDirectory::open_absolute(root)?;
    root.validate_destination()?;
    super::destination_security::check_private_file(&root.file)?;
    exact_children(&root, &[PLAN_FILE, PROFILES_DIRECTORY])?;
    let storage =
        StorageDirectory::from_file(super::migration_durability::readable_directory(&root.file)?)?;
    use std::io::Read;
    let mut bytes = Vec::new();
    storage
        .open_read(PLAN_FILE.as_ref(), Privacy::OwnerOnly)?
        .take((browser_continuity::MAX_PLAN_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if Plan::decode(&bytes).map_err(io::Error::other)? != *plan {
        return Err(changed("Browser projection plan differs"));
    }
    let metadata = super::nofollow::metadata_nofollow(&root.file, Path::new(PROFILES_DIRECTORY))?;
    let profiles = root.open_child(PROFILES_DIRECTORY.as_ref(), &metadata)?;
    let ids = plan
        .profiles
        .iter()
        .map(|profile| profile.id.as_str())
        .collect::<Vec<_>>();
    exact_children(&profiles, &ids)?;
    let mut total = 0u64;
    for profile in &plan.profiles {
        check_cancel(cancel)?;
        let metadata = super::nofollow::metadata_nofollow(&profiles.file, Path::new(&profile.id))?;
        let directory = profiles.open_child(profile.id.as_ref(), &metadata)?;
        exact_children(&directory, profile.allowed_files())?;
        let snapshot = browser_continuity::snapshot::capture(
            super::migration_durability::readable_directory(&directory.file)?,
            profile,
        )?;
        total = total
            .checked_add(snapshot.bytes())
            .filter(|total| *total <= MAX_BYTES)
            .ok_or_else(|| changed("Browser projection exceeds its transfer bound"))?;
    }
    root.validate()
}

impl crate::staging::browser::Expectation for Expectation {
    fn validate(&self) -> std::io::Result<()> {
        self.validate()
    }
    fn filename(&self) -> &str {
        &self.filename
    }
    fn sealed_bytes(&self) -> u64 {
        self.sealed_bytes
    }
    fn ciphertext_sha256(&self) -> [u8; 32] {
        self.ciphertext_sha256
    }
}

#[cfg(test)]
mod tests;
