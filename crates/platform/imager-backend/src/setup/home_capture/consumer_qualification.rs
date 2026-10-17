//! One installed, fixed disposable-subject consumer probe. This never creates
//! source grants, credentials or a setup declaration from a fixture DTO.
use super::{failure, offline_output::bootstrap, runtime};
use crate::setup::migration;
use cap_fs_ext::OpenOptionsFollowExt as _;
use launcher::owned_service::ServiceCancellation;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{FileExt as _, MetadataExt},
    },
    path::Path,
    sync::atomic::AtomicBool,
    thread,
    time::Duration,
};
use zeroize::Zeroizing;

// This public fixture password decrypts only the synthetic pending selection
// prepared by the owned qualification scenario. It authorizes no source: the
// protected same-source records and actual original-user grants do that.
const PASSWORD: &[u8] = b"several isolated synthetic qualification words";
const SOURCE_MARKER: &[u8] = b"disposable-original-source-only";
const CONFIG_MARKER: &[u8] = b"disposable-paired-config-only";

/// Constructor and all fields are private. Callers cannot deserialize a saved
/// boolean, a test log, or a worker advertisement into successful qualification.
pub struct QualifiedOriginalConsumer {
    receipt: [u8; 32],
    pins: [u8; 32],
    receiving: Option<super::applications_qualification::QualifiedApplicationsConsumer>,
}
impl QualifiedOriginalConsumer {
    pub fn receiving_proof(&self) -> io::Result<&serde_json::Value> {
        self.receiving
            .as_ref()
            .map(|proof| proof.proof())
            .ok_or_else(|| failure("Actual application receiving qualification missing"))
    }
    pub fn receipt_sha256(&self) -> [u8; 32] {
        self.receipt
    }
    pub fn capture_pins_sha256(&self) -> [u8; 32] {
        self.pins
    }
}
fn exact_marker(file: &File, expected: &[u8]) -> io::Result<()> {
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != 0
        || before.nlink() != 1
        || before.mode() & 0o7777 != 0o600
        || before.len() != expected.len() as u64
    {
        return Err(failure("Qualification ownership marker is not protected"));
    }
    let mut bytes = vec![0; expected.len() + 1];
    let mut offset = 0;
    while offset < bytes.len() {
        let n = file.read_at(&mut bytes[offset..], offset as u64)?;
        if n == 0 {
            break;
        }
        offset += n;
    }
    bytes.truncate(offset);
    let after = file.metadata()?;
    if bytes != expected
        || (
            before.dev(),
            before.ino(),
            before.mode(),
            before.uid(),
            before.gid(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ) != (
            after.dev(),
            after.ino(),
            after.mode(),
            after.uid(),
            after.gid(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        )
    {
        return Err(failure("Qualification ownership marker changed"));
    }
    Ok(())
}
fn open_marker(root: &File, name: &str, expected: &[u8]) -> io::Result<()> {
    if root.metadata()?.uid() != 0 || root.metadata()?.mode() & 0o022 != 0 {
        return Err(failure("Qualification filesystem root is not protected"));
    }
    let mut options = cap_primitives::fs::OpenOptions::new();
    options
        .read(true)
        .follow(cap_primitives::fs::FollowSymlinks::No);
    let file = cap_primitives::fs::open(root, Path::new(name), &options)?;
    if fs_native::snapshot_lease::mount_id(root)? != fs_native::snapshot_lease::mount_id(&file)? {
        return Err(failure("Qualification marker crosses an undeclared mount"));
    }
    exact_marker(&file, expected)
}
/// Called only on the real resolver/participant's independently validated full
/// readonly source root. A marker proves disposable ownership, never a grant.
pub fn check_source_root_marker(root: &File) -> io::Result<()> {
    open_marker(root, ".tilo-original-source-qualification", SOURCE_MARKER)
}
fn private_config() -> io::Result<()> {
    let root = migration::PinnedDirectory::open_absolute(Path::new("/etc"))?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(root.file.as_raw_fd(), &raw mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if stat.f_type != libc::TMPFS_MAGIC {
        return Err(failure(
            "Qualification requires its owned private configuration mount",
        ));
    }
    open_marker(&root.file, ".tilo-paired-qualification", CONFIG_MARKER)?;
    root.validate()
}
/// No paths, IDs, artifact environment overrides or peer-authored claims are
/// accepted. The scenario has already used actual original-owner grants and
/// canonical registration to seed the fixed authenticated pending inbox.
pub fn run_owned_disposable(cancel: &AtomicBool) -> io::Result<QualifiedOriginalConsumer> {
    run_owned(cancel, false)
}
/// Receiving support is derived only after the actual source6 factory, target
/// restore/retry and exact receiving namespace retirement all succeed.
pub fn run_owned_applications_disposable(
    cancel: &AtomicBool,
) -> io::Result<QualifiedOriginalConsumer> {
    run_owned(cancel, true)
}
fn run_owned(cancel: &AtomicBool, receiving: bool) -> io::Result<QualifiedOriginalConsumer> {
    if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
        return Err(failure(
            "Original consumer qualification requires the installed root controller",
        ));
    }
    private_config()?;
    migration::check_cancel(cancel)?;
    let (_, compiled, pins) = runtime::installed_original_consumer_facts()?;
    let protocol = compiled.worker_protocol;
    if !matches!(protocol, 5 | 6) || (receiving && protocol != 6) {
        return Err(failure(
            "Original qualification requires the exact installed protocol5 or6 cohort",
        ));
    }
    let targets =
        crate::probe::probe().ok_or_else(|| failure("Qualification disk discovery failed"))?;
    let targets = targets
        .into_iter()
        .filter(|target| {
            !target.is_system
                && target.tran.as_deref() == Some("usb")
                && target.stable_id.as_deref() == Some("tilo-install-target")
        })
        .collect::<Vec<_>>();
    let [target] = targets.as_slice() else {
        return Err(failure(
            "Exactly one actual owned USB qualification medium is required",
        ));
    };
    let daemon = block_devices::udisks::Udisks::system().map_err(io::Error::other)?;
    let raw = daemon
        .open_read_only(&target.dev_path())
        .map_err(io::Error::other)?;
    crate::setup::read_media::attest_boot_descriptor(target, &raw)?;
    let cancellation = ServiceCancellation::default();
    let worker_cancel = cancellation.clone();
    let evidence = thread::scope(|scope| {
        let job = scope.spawn(|| {
            crate::write::with_locked_media_root(target, || {
                if protocol == 6 {
                    let evidence = bootstrap::qualify_applications_owned(
                        target, &raw, Zeroizing::new(PASSWORD.to_vec()), cancel, worker_cancel,
                    )?;
                    let target_proof=if receiving {
                        Some(super::applications_qualification::consume(target,&raw,&evidence,cancel)?)
                    } else {None};
                    Ok::<_, io::Error>((serde_json::json!({
                        "schema": 1, "original_source_protocol": 6,
                        "capture_pins_sha256": pins,
                        "source_disk_guids": evidence.source_disk_guids,
                        "read_claim_sha256": evidence.read_claim_sha256,
                        "applications_ciphertext_sha256": evidence.applications.ciphertext_sha256,
                        "completion_receipt_sha256": evidence.completion_receipt_sha256,
                    }),target_proof))
                } else {
                    let evidence = bootstrap::qualify_owned(
                        target, &raw, Zeroizing::new(PASSWORD.to_vec()), cancel, worker_cancel,
                    )?;
                    Ok::<_, io::Error>((serde_json::json!({
                        "schema": 1, "original_source_protocol": 5,
                        "capture_pins_sha256": pins,
                        "source_disk_guids": evidence.source_disk_guids,
                        "read_claim_sha256": evidence.read_claim_sha256,
                        "home_ciphertext_sha256": evidence.home.ciphertext_sha256,
                        "applications_ciphertext_sha256": evidence.applications.ciphertext_sha256,
                        "completion_receipt_sha256": evidence.completion_receipt_sha256,
                    }),None))
                }
            })
        });
        while !job.is_finished() {
            if migration::check_cancel(cancel).is_err() {
                cancellation.cancel();
            }
            thread::sleep(Duration::from_millis(25));
        }
        job.join()
            .map_err(|_| failure("Original qualification transaction panicked"))?
    })?;
    migration::check_cancel(cancel)?;
    private_config()?;
    crate::setup::read_media::attest_boot_descriptor(target, &raw)?;
    let (_, _, after) = runtime::installed_original_consumer_facts()?;
    if after != pins {
        return Err(failure(
            "Installed original consumer artifacts changed during qualification",
        ));
    }
    // The shared transaction returns only after both real independent namespaces
    // and all source/output mount leases have observed successful retirement.
    let receipt = Sha256::digest(serde_json::to_vec(&evidence.0)?).into();
    Ok(QualifiedOriginalConsumer {
        receipt,
        pins,
        receiving: evidence.1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    #[test]
    fn ownership_marker_refuses_wrong_bytes_shape_links_and_owner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("marker");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        use std::io::Write as _;
        file.try_clone().unwrap().write_all(SOURCE_MARKER).unwrap();
        assert_eq!(
            exact_marker(&file, SOURCE_MARKER).is_ok(),
            rustix::process::geteuid().as_raw() == 0,
            "Only the actual root-owned correctly shaped file is accepted"
        );
        let mut different = SOURCE_MARKER.to_vec();
        different[0] ^= 1;
        assert!(exact_marker(&file, &different).is_err());
        assert!(exact_marker(&file, CONFIG_MARKER).is_err());
        std::fs::hard_link(&path, directory.path().join("alias")).unwrap();
        assert!(exact_marker(&file, SOURCE_MARKER).is_err());
        std::fs::remove_file(directory.path().join("alias")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(exact_marker(&file, SOURCE_MARKER).is_err());
    }
}
