//! Native preflight for the public TPM sidecar a prepared setup carries.
//!
//! The wire format itself lives in [`staging_handoff_model::prepared`], shared
//! by the preparation side and the installed boot. This module adds the checks
//! both the installer's extractor and the privileged first-boot helper run
//! against it: the sidecar is a bounded, untampered regular file reached
//! through a held parent directory without following any link, and it names
//! the exact package bytes read under the same descriptor identity checks.
//! It stays a platform leaf so both callers import one parser and one set of
//! guards without either depending on the installer backend or carrying its
//! peer transport.
//!
//! This is deliberately **not** an authentication boundary for the release
//! authority. The shared wire model validates the sidecar's structure and its
//! public binding digest; the signed target policy is verified only by the
//! privileged unsealer against the release authority pinned into that helper,
//! before it releases the package secret. The policy fields are public input
//! from untrusted media, never authority to mutate the target. Callers that
//! need the authenticated policy must unseal, not widen this preflight.

use std::ffi::{CString, OsStr};
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Component, Path};

use sha2::{Digest as _, Sha256};
use staging_handoff_model::prepared::{MAX_SIDECAR, SealedPreparation};

/// The canonical prepared package is at most 32 MiB + 16 KiB. A preflight
/// reads at most this much even when a caller names a larger file.
const MAX_PACKAGE: u64 = 32 * 1024 * 1024 + 16 * 1024;
const SIDECAR_NOT_BOUNDED: &str = "the prepared TPM handoff is not a bounded regular file";
const SIDECAR_CHANGED: &str = "the prepared TPM handoff changed while it was read";
const PACKAGE_NOT_BOUNDED: &str = "the prepared package is not a bounded regular file";
const PACKAGE_CHANGED: &str = "the prepared package changed while it was read";
const ANOTHER_PACKAGE: &str = "the prepared handoff names another package";
const UNSAFE_PATH: &str = "the prepared handoff path is not a plain path";

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

impl Stamp {
    fn of(metadata: &Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            len: metadata.len(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

/// Verify a sidecar and the exact package it names before first boot.
///
/// The check is structural: it refuses a linked, edited, truncated, growing or
/// oversized sidecar and a package whose bytes or identity change while they
/// are read. It does not verify the signed target policy; the privileged
/// unsealer does that against its pinned release authority before releasing
/// any secret.
///
/// # Errors
///
/// The sidecar or package is not a bounded no-follow regular file reached
/// through a plain path, it changes while it is read, the sidecar is malformed
/// or edited, or it names another package digest.
pub fn preflight_handoff(sidecar_path: &Path, package_path: &Path) -> io::Result<()> {
    let _sealed = read_prepared_handoff(sidecar_path, package_path)?;
    Ok(())
}

/// Read the sidecar and verify that it names the exact package bytes beside it.
///
/// This returns the decoded sidecar a privileged caller should hand to unseal,
/// so the policy that is authenticated is exactly the one this preflight read.
/// It does not authenticate the policy signature.
///
/// # Errors
///
/// The sidecar or package is not a bounded no-follow regular file reached
/// through a plain path, it changes while it is read, the sidecar is malformed
/// or edited, or it names another package digest.
pub fn read_prepared_handoff(
    sidecar_path: &Path,
    package_path: &Path,
) -> io::Result<SealedPreparation> {
    let sidecar = read_stable(
        sidecar_path,
        MAX_SIDECAR,
        SIDECAR_NOT_BOUNDED,
        SIDECAR_CHANGED,
    )?;
    let sealed = SealedPreparation::decode(&sidecar)?;
    let digest = digest_stable(
        package_path,
        MAX_PACKAGE,
        PACKAGE_NOT_BOUNDED,
        PACKAGE_CHANGED,
    )?;
    if sealed.package_sha256() != digest {
        return Err(invalid(ANOTHER_PACKAGE));
    }
    Ok(sealed)
}

fn read_stable(
    path: &Path,
    limit: u64,
    not_bounded: &'static str,
    changed: &'static str,
) -> io::Result<Vec<u8>> {
    let (mut file, before) = open_pinned(path, limit, not_bounded)?;
    read_held(&mut file, &before, limit, changed)
}

fn digest_stable(
    path: &Path,
    limit: u64,
    not_bounded: &'static str,
    changed: &'static str,
) -> io::Result<[u8; 32]> {
    let (mut file, before) = open_pinned(path, limit, not_bounded)?;
    digest_held(&mut file, &before, limit, changed)
}

fn read_held(
    file: &mut File,
    before: &Metadata,
    limit: u64,
    changed: &'static str,
) -> io::Result<Vec<u8>> {
    let bytes = read_exact_bounded(&mut *file, before.len(), limit, changed)?;
    if Stamp::of(before) != Stamp::of(&file.metadata()?) {
        return Err(invalid(changed));
    }
    Ok(bytes)
}

fn digest_held(
    file: &mut File,
    before: &Metadata,
    limit: u64,
    changed: &'static str,
) -> io::Result<[u8; 32]> {
    let digest = digest_exact_bounded(&mut *file, before.len(), limit, changed)?;
    if Stamp::of(before) != Stamp::of(&file.metadata()?) {
        return Err(invalid(changed));
    }
    Ok(digest)
}

fn read_exact_bounded(
    reader: impl Read,
    expected: u64,
    limit: u64,
    changed: &'static str,
) -> io::Result<Vec<u8>> {
    if expected > limit {
        return Err(invalid(changed));
    }
    let mut bytes = Vec::new();
    reader
        .take(expected.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != expected {
        return Err(invalid(changed));
    }
    Ok(bytes)
}

fn digest_exact_bounded(
    mut reader: impl Read,
    expected: u64,
    limit: u64,
    changed: &'static str,
) -> io::Result<[u8; 32]> {
    if expected > limit {
        return Err(invalid(changed));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 65536].into_boxed_slice();
    let mut total = 0_u64;
    while total < expected {
        let remaining = expected.saturating_sub(total);
        let want = usize::try_from(remaining)
            .unwrap_or(buffer.len())
            .min(buffer.len());
        let count = reader.read(buffer.get_mut(..want).ok_or_else(|| invalid(changed))?)?;
        if count == 0 {
            return Err(invalid(changed));
        }
        total = total.saturating_add(count as u64);
        if total > expected {
            return Err(invalid(changed));
        }
        hasher.update(buffer.get(..count).ok_or_else(|| invalid(changed))?);
    }
    // The reviewed length is a bound, not a hint: one byte beyond it is enough
    // to refuse a source that grew after its metadata was observed.
    let mut extra = [0_u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(invalid(changed));
    }
    Ok(hasher.finalize().into())
}

fn open_pinned(path: &Path, limit: u64, not_bounded: &'static str) -> io::Result<(File, Metadata)> {
    let file = open_no_follow(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid(not_bounded));
    }
    Ok((file, metadata))
}

fn open_no_follow(path: &Path) -> io::Result<File> {
    let name = path.file_name().ok_or_else(|| invalid(UNSAFE_PATH))?;
    let parent_path = path.parent().ok_or_else(|| invalid(UNSAFE_PATH))?;
    let root = if parent_path.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let mut parent = open_directory(root)?;
    for component in parent_path.components() {
        match component {
            Component::RootDir | Component::CurDir => (),
            Component::Normal(name) => parent = open_component(&parent, name, true)?,
            Component::ParentDir | Component::Prefix(_) => return Err(invalid(UNSAFE_PATH)),
        }
    }
    open_component(&parent, name, false)
}

fn open_directory(path: &Path) -> io::Result<File> {
    File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

fn open_component(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| invalid(UNSAFE_PATH))?;
    let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a failed fixture call fails the test; fixtures are small known values"
    )]

    use super::*;
    use staging_handoff_model::prepared::{
        PCRS, PLATFORM_PCRS, RECEIVER_UID, SCHEMA, SignedTarget, b64,
    };

    fn encoded_sidecar(package_sha256: [u8; 32]) -> Vec<u8> {
        let policy = SignedTarget {
            schema: SCHEMA,
            generation: 1,
            pcr_bank: "sha256".into(),
            pcrs: PCRS.to_vec(),
            platform_pcrs: PLATFORM_PCRS.to_vec(),
            approved_policy: b64(&[1; 32]),
            signature: b64(&[2; 256]),
            target_uki_sha256: [3; 32],
            authority_sha256: [4; 32],
            label: "preflight fixture".into(),
        };
        let mut sealed = SealedPreparation {
            schema: SCHEMA,
            policy,
            source_public_key: [5; 32],
            package_sha256,
            receiver_uid: RECEIVER_UID,
            receiver_tpm_name: b64(&[6; 32]),
            key_name: b64(&[7; 32]),
            sealed_public: b64(&[8; 32]),
            sealed_private: b64(&[9; 32]),
            policy_digest: b64(&[10; 32]),
            seal_nonce: b64(&[11; 32]),
            binding_sha256: String::new(),
        };
        sealed.seal_binding().unwrap();
        sealed.encode().unwrap()
    }

    #[test]
    fn the_sidecar_must_name_the_exact_package_bytes() {
        let package = b"the exact canonical prepared package";
        let directory = tempfile::tempdir().unwrap();
        let sidecar = directory.path().join("tpm-handoff.json");
        let package_path = directory.path().join("prepared-setup.sealed");
        std::fs::write(&sidecar, encoded_sidecar(Sha256::digest(package).into())).unwrap();
        std::fs::write(&package_path, package).unwrap();
        preflight_handoff(&sidecar, &package_path).unwrap();

        std::fs::write(&package_path, b"another canonical prepared package").unwrap();
        let error = preflight_handoff(&sidecar, &package_path)
            .expect_err("a sidecar for another package must be refused");
        assert!(error.to_string().contains("another package"));
    }

    #[test]
    fn a_linked_or_edited_sidecar_is_refused() {
        let package = b"the exact canonical prepared package";
        let directory = tempfile::tempdir().unwrap();
        let sidecar = directory.path().join("tpm-handoff.json");
        let linked = directory.path().join("linked.json");
        let package_path = directory.path().join("prepared-setup.sealed");
        std::fs::write(&sidecar, encoded_sidecar(Sha256::digest(package).into())).unwrap();
        std::fs::write(&package_path, package).unwrap();
        std::os::unix::fs::symlink(&sidecar, &linked).unwrap();
        assert!(preflight_handoff(&linked, &package_path).is_err());

        let mut edited = encoded_sidecar(Sha256::digest(package).into());
        let field = b"\"seal_nonce\"";
        let position = edited
            .windows(field.len())
            .position(|window| window == field)
            .unwrap();
        let digit = position
            + edited[position..]
                .iter()
                .position(u8::is_ascii_digit)
                .unwrap();
        edited[digit] = if edited[digit] == b'0' { b'1' } else { b'0' };
        std::fs::write(&sidecar, edited).unwrap();
        assert!(preflight_handoff(&sidecar, &package_path).is_err());
    }

    fn fixture(directory: &Path, package: &[u8]) -> (std::path::PathBuf, std::path::PathBuf) {
        let sidecar = directory.join("tpm-handoff.json");
        let package_path = directory.join("prepared-setup.sealed");
        std::fs::write(&sidecar, encoded_sidecar(Sha256::digest(package).into())).unwrap();
        std::fs::write(&package_path, package).unwrap();
        (sidecar, package_path)
    }

    #[test]
    fn an_oversized_package_is_refused_before_it_is_read() {
        let directory = tempfile::tempdir().unwrap();
        let (sidecar, package_path) = fixture(directory.path(), b"a package");
        std::fs::File::create(&package_path)
            .unwrap()
            .set_len(MAX_PACKAGE.saturating_add(1))
            .unwrap();
        let error = preflight_handoff(&sidecar, &package_path)
            .expect_err("an oversized package must be refused");
        assert!(error.to_string().contains("bounded regular file"));
    }

    #[test]
    fn an_oversized_sidecar_is_refused_before_it_is_read() {
        let directory = tempfile::tempdir().unwrap();
        let (sidecar, package_path) = fixture(directory.path(), b"a package");
        std::fs::File::create(&sidecar)
            .unwrap()
            .set_len(MAX_SIDECAR.saturating_add(1))
            .unwrap();
        let error = preflight_handoff(&sidecar, &package_path)
            .expect_err("an oversized sidecar must be refused");
        assert!(error.to_string().contains("bounded regular file"));
    }

    #[test]
    fn a_package_that_grows_while_it_is_hashed_is_refused() {
        let expected = 8_u64;
        let mut bytes = vec![0_u8; usize::try_from(expected).unwrap()];
        bytes.push(1);
        let error = digest_exact_bounded(
            std::io::Cursor::new(bytes),
            expected,
            MAX_PACKAGE,
            PACKAGE_CHANGED,
        )
        .expect_err("a growing package must be refused");
        assert!(error.to_string().contains("changed"));
    }

    #[test]
    fn a_package_that_shrinks_while_it_is_hashed_is_refused() {
        let expected = 8_u64;
        let bytes = vec![0_u8; usize::try_from(expected.saturating_sub(1)).unwrap()];
        let error = digest_exact_bounded(
            std::io::Cursor::new(bytes),
            expected,
            MAX_PACKAGE,
            PACKAGE_CHANGED,
        )
        .expect_err("a truncated package must be refused");
        assert!(error.to_string().contains("changed"));
    }

    #[test]
    fn a_sidecar_that_grows_while_it_is_read_is_refused() {
        let expected = 4_u64;
        let mut bytes = vec![0_u8; usize::try_from(expected).unwrap()];
        bytes.push(1);
        let error = read_exact_bounded(
            std::io::Cursor::new(bytes),
            expected,
            MAX_SIDECAR,
            SIDECAR_CHANGED,
        )
        .expect_err("a growing sidecar must be refused");
        assert!(error.to_string().contains("changed"));
    }

    #[test]
    fn a_truncated_sidecar_file_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let package = b"the exact canonical prepared package";
        let (sidecar, package_path) = fixture(directory.path(), package);
        let full = std::fs::read(&sidecar).unwrap();
        std::fs::write(&sidecar, full.get(..1).unwrap()).unwrap();
        assert!(preflight_handoff(&sidecar, &package_path).is_err());
    }

    #[test]
    fn a_package_changed_after_it_is_opened_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let package = b"the exact canonical prepared package";
        let (sidecar, package_path) = fixture(directory.path(), package);
        let (mut held, before) =
            open_pinned(&package_path, MAX_PACKAGE, PACKAGE_NOT_BOUNDED).unwrap();
        std::fs::write(&package_path, b"another package, another length").unwrap();
        let error = digest_held(&mut held, &before, MAX_PACKAGE, PACKAGE_CHANGED)
            .expect_err("a package changed after opening must be refused");
        assert!(error.to_string().contains("changed"));
        assert!(preflight_handoff(&sidecar, &package_path).is_err());
    }

    #[test]
    fn a_sidecar_changed_after_it_is_opened_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let package = b"the exact canonical prepared package";
        let (sidecar, _package_path) = fixture(directory.path(), package);
        let (mut held, before) = open_pinned(&sidecar, MAX_SIDECAR, SIDECAR_NOT_BOUNDED).unwrap();
        std::fs::write(&sidecar, encoded_sidecar([9; 32])).unwrap();
        let error = read_held(&mut held, &before, MAX_SIDECAR, SIDECAR_CHANGED)
            .expect_err("a sidecar changed after opening must be refused");
        assert!(error.to_string().contains("changed"));
    }

    #[test]
    fn a_linked_parent_directory_is_not_followed_for_the_sidecar() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let (_sidecar, package_path) = fixture(&real, b"the exact package");
        let linked = directory.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        assert!(preflight_handoff(&linked.join("tpm-handoff.json"), &package_path).is_err());
    }

    #[test]
    fn a_linked_parent_directory_is_not_followed_for_the_package() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let (sidecar, _package_path) = fixture(&real, b"the exact package");
        let linked = directory.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        assert!(preflight_handoff(&sidecar, &linked.join("prepared-setup.sealed")).is_err());
    }

    #[test]
    fn a_linked_package_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let (sidecar, package_path) = fixture(directory.path(), b"the exact package");
        let linked = directory.path().join("linked.sealed");
        std::os::unix::fs::symlink(&package_path, &linked).unwrap();
        assert!(preflight_handoff(&sidecar, &linked).is_err());
    }

    #[test]
    fn a_parent_component_traversal_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let (_sidecar, package_path) = fixture(directory.path(), b"the exact package");
        let nested = directory.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let traversed = nested.join("..").join("tpm-handoff.json");
        let error = preflight_handoff(&traversed, &package_path)
            .expect_err("a path with a parent component must be refused");
        assert!(error.to_string().contains("plain path"));
    }

    #[test]
    fn the_shared_reader_returns_the_sidecar_it_verified() {
        let directory = tempfile::tempdir().unwrap();
        let package = b"the exact canonical prepared package";
        let (sidecar, package_path) = fixture(directory.path(), package);
        let sealed = read_prepared_handoff(&sidecar, &package_path).unwrap();
        assert_eq!(
            sealed.package_sha256(),
            <[u8; 32]>::from(Sha256::digest(package))
        );
    }
}
