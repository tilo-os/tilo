//! Installed selected-app receiving qualification on owned disposable material.
//! The sole source is an opaque successful original6 completed transaction.
mod child;
mod process;
use super::offline_output::{
    bootstrap::CompletedApplicationsQualificationEvidence, with_boot_medium,
};
use crate::setup::{
    migration::PinnedDirectory, prepared_setup::completion::boot_review::CompletedBootReview,
};
use sha2::{Digest as _, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    },
    path::Path,
    sync::atomic::AtomicBool,
};
const ROOT: &str = "/run/tilo-applications-qualification";
const HELPER: &str = "/usr/libexec/tilo-prepared-setup";
const PASSWORD: &[u8] = b"several isolated synthetic qualification words";
const MARKER: &[u8] = b"owned-installed-applications-consumer-qualification";
const SELECTED_BYTES: &[u8] = b"genuine selected application state";
const BOUND: usize = 4096;
fn invalid() -> io::Error {
    io::Error::other("Installed application consumer qualification failed")
}
/// No Deserialize or public constructor: successful child work and kernel
/// namespace retirement, not a public receipt, create this evidence.
pub(super) struct QualifiedApplicationsConsumer {
    proof: serde_json::Value,
}
impl QualifiedApplicationsConsumer {
    pub(super) fn proof(&self) -> &serde_json::Value {
        &self.proof
    }
}
fn protected(path: &Path) -> io::Result<File> {
    for edge in path.ancestors() {
        let metadata = fs::symlink_metadata(edge)?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 || metadata.file_type().is_symlink()
        {
            return Err(invalid());
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() == 0
        || metadata.len() > 1024 * 1024 * 1024
    {
        return Err(invalid());
    }
    Ok(file)
}
fn digest(file: &File) -> io::Result<String> {
    use std::os::unix::fs::FileExt as _;
    let before = file.metadata()?;
    let mut offset = 0;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read_at(&mut buffer, offset)?;
        if n == 0 {
            break;
        }
        offset += n as u64;
        if offset > 1024 * 1024 * 1024 {
            return Err(invalid());
        }
        hash.update(&buffer[..n]);
    }
    let after = file.metadata()?;
    if (
        before.dev(),
        before.ino(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) {
        return Err(invalid());
    }
    Ok(hex::encode(hash.finalize()))
}
struct OwnedRoot {
    root: std::sync::Arc<PinnedDirectory>,
    retired: bool,
}
impl OwnedRoot {
    fn create() -> io::Result<Self> {
        let parent = PinnedDirectory::open_absolute(Path::new("/run"))?;
        parent.validate()?;
        let metadata = parent.file.metadata()?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(invalid());
        }
        // mkdir is atomic and refuses every existing object; no reuse or cleanup
        // of another qualification's directory is permitted.
        let name = c"tilo-applications-qualification";
        if unsafe { libc::mkdirat(parent.file.as_raw_fd(), name.as_ptr(), 0o711) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let root = PinnedDirectory::open_absolute(Path::new(ROOT))?;
        fs::set_permissions(ROOT, fs::Permissions::from_mode(0o711))?;
        let value = Self {
            root,
            retired: false,
        };
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> io::Result<()> {
        self.root.validate_destination()?;
        let m = self.root.file.metadata()?;
        if m.uid() != 0 || m.gid() != 0 || m.mode() & 0o7777 != 0o711 {
            return Err(invalid());
        }
        Ok(())
    }
    fn finish(mut self) -> io::Result<()> {
        self.validate()?;
        self.retired = true;
        fs::remove_dir_all(ROOT)
    }
}
impl Drop for OwnedRoot {
    fn drop(&mut self) {
        // On an uncertain child exit leave this precisely owned subtree retained.
        // Never delete files while an unobserved receiving writer might still run.
    }
}
fn directory(path: &Path, mode: u32, owner: Option<(u32, u32)>) -> io::Result<()> {
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    let pin = PinnedDirectory::open_absolute(path)?;
    if let Some((uid, gid)) = owner {
        let writable = crate::setup::migration_durability::readable_directory(&pin.file)?;
        if unsafe { libc::fchown(writable.as_raw_fd(), uid, gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    pin.validate_destination()
}
fn output(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}
fn copy_setup(
    source: &Path,
    destination: &Path,
    expected: &crate::imager_model::PreparedSetupExpectation,
) -> io::Result<()> {
    let parent = PinnedDirectory::open_absolute(source.parent().ok_or_else(invalid)?)?;
    let metadata = parent.file.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o7777 != 0o700 {
        return Err(invalid());
    }
    let storage = tilo_files::native::atomic::StorageDirectory::from_file(
        crate::setup::migration_durability::readable_directory(&parent.file)?,
    )?;
    let mut source = storage.open_read(
        source.file_name().ok_or_else(invalid)?,
        tilo_files::native::atomic::Privacy::OwnerOnly,
    )?;
    let mut destination = output(destination)?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes = bytes.checked_add(n as u64).ok_or_else(invalid)?;
        if bytes > expected.sealed_bytes {
            return Err(invalid());
        }
        hash.update(&buffer[..n]);
        destination.write_all(&buffer[..n])?;
    }
    let actual = hex::encode(hash.finalize());
    if bytes != expected.sealed_bytes || actual != expected.ciphertext_sha256 {
        return Err(invalid());
    }
    parent.validate_destination()?;
    destination.sync_all()?;
    destination.set_permissions(fs::Permissions::from_mode(0o444))
}
/// The source type cannot be reconstructed from a saved JSON qualification.
/// The exact completed capsule and ciphertext are independently reacquired under
/// genuine selected-medium custody before the receiving namespace is launched.
pub(super) fn consume(
    target: &crate::imager_model::TargetDrive,
    raw: &File,
    evidence: &CompletedApplicationsQualificationEvidence,
    cancel: &AtomicBool,
) -> io::Result<QualifiedApplicationsConsumer> {
    crate::setup::migration::check_cancel(cancel)?;
    let owned = OwnedRoot::create()?;
    let artifacts = Path::new(ROOT).join("artifacts");
    directory(&artifacts, 0o755, None)?;
    let mut marker = output(&Path::new(ROOT).join("owner"))?;
    marker.write_all(MARKER)?;
    marker.sync_all()?;
    marker.set_permissions(fs::Permissions::from_mode(0o444))?;
    let receiving = Path::new(ROOT).join("receiving");
    directory(&receiving, 0o700, Some((1000, 1001)))?;
    with_boot_medium(target, raw, cancel, |medium| {
        let committed = CompletedBootReview::review_if_committed(medium, PASSWORD, cancel)?
            .ok_or_else(invalid)?;
        let capsule = committed.retained_setup(cancel)?;
        if capsule.expectation() != &evidence.setup
            || !capsule
                .authenticated_setup()
                .manifest()
                .completed_applications_only
            || capsule
                .authenticated_setup()
                .manifest()
                .portable_flatpak
                .as_ref()
                != Some(&evidence.applications)
        {
            return Err(invalid());
        }
        copy_setup(
            &capsule.package_path(),
            &artifacts.join("prepared-setup.sealed"),
            &evidence.setup,
        )?;
        let mut destination = output(&artifacts.join(&evidence.applications.filename))?;
        committed.stream_qualification_applications(
            &evidence.applications,
            &mut destination,
            cancel,
        )?;
        destination.sync_all()?;
        destination.set_permissions(fs::Permissions::from_mode(0o444))?;
        capsule.validate()?;
        committed.validate(cancel)
    })?; // actual readonly medium retirement precedes the receiving UID's writes
    let mut expected = output(&artifacts.join("expected.json"))?;
    serde_json::to_writer(&mut expected, &evidence.applications)?;
    expected.sync_all()?;
    expected.set_permissions(fs::Permissions::from_mode(0o444))?;
    owned.validate()?;
    let helper = protected(Path::new(HELPER))?;
    let helper_sha = digest(&helper)?;
    let tools = [
        "/usr/bin/flatpak",
        "/usr/bin/ostree",
        "/usr/bin/unshare",
        "/usr/bin/setpriv",
    ];
    let mut held = Vec::new();
    let mut tool_hashes = serde_json::Map::new();
    for path in tools {
        let file = protected(Path::new(path))?;
        tool_hashes.insert(path.into(), digest(&file)?.into());
        held.push(file);
    }
    let report = process::run(&helper, cancel)?;
    owned.validate()?;
    if digest(&helper)? != helper_sha {
        return Err(invalid());
    }
    for (path, file) in tools.iter().zip(&held) {
        if tool_hashes.get(*path).and_then(serde_json::Value::as_str)
            != Some(digest(file)?.as_str())
        {
            return Err(invalid());
        }
    }
    let proof = serde_json::json!({"schema":1,"subset_protocol":2,"source_protocol":6,
        "prepared_helper_sha256":helper_sha,"tools":tool_hashes,
        "source_read_claim_sha256":evidence.read_claim_sha256,
        "source_completion_receipt_sha256":evidence.completion_receipt_sha256,
        "setup_ciphertext_sha256":evidence.setup.ciphertext_sha256,
        "applications_ciphertext_sha256":evidence.applications.ciphertext_sha256,
        "receiving":report});
    // No evidence escapes until the real PID namespace is dead and the owned
    // receiving repo/storage subtree has been removed through its held edge.
    owned.finish()?;
    crate::setup::migration::check_cancel(cancel)?;
    Ok(QualifiedApplicationsConsumer { proof })
}
/// Fixed child dispatcher; never accepts a source path, password or proof DTO.
pub fn run_child(
    args: impl IntoIterator<Item = std::ffi::OsString>,
    output: &mut impl Write,
) -> io::Result<()> {
    if args.into_iter().next().is_some() {
        return Err(invalid());
    }
    child::run(output)
}
