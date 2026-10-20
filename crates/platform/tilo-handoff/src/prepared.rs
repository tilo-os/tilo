//! Native preflight for the public TPM sidecar a prepared setup carries.
//!
//! The wire format itself lives in [`staging_handoff_model::prepared`], shared
//! by the preparation side and the installed boot. This module adds the one
//! check both the installer's extractor and the privileged first-boot helper
//! run against it: the sidecar is a bounded, untampered regular file and it
//! names the exact package bytes beside it. It stays a platform leaf so both
//! callers import one parser and one set of guards without either depending on
//! the installer backend or carrying its peer transport.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use sha2::{Digest as _, Sha256};
use staging_handoff_model::prepared::{MAX_SIDECAR, SealedPreparation};

/// The canonical prepared package is at most 32 MiB + 16 KiB. A preflight
/// reads at most this much even when a caller names a larger file.
const MAX_PACKAGE: u64 = 32 * 1024 * 1024 + 16 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Verify a sidecar and the exact package it names before first boot.
///
/// # Errors
///
/// The sidecar or package is not a bounded no-follow regular file, the sidecar
/// is malformed or edited, or it names another package digest.
pub fn preflight_handoff(sidecar_path: &Path, package_path: &Path) -> io::Result<()> {
    let sidecar = read_bounded(sidecar_path, MAX_SIDECAR)?;
    let sealed = SealedPreparation::decode(&sidecar)?;
    let mut package = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(package_path)?;
    let before = package.metadata()?;
    if !before.is_file() || before.len() > MAX_PACKAGE {
        return Err(invalid(
            "the prepared package is not a bounded regular file",
        ));
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 65536].into_boxed_slice();
    loop {
        let count = package.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(
            buffer
                .get(..count)
                .ok_or_else(|| invalid("the prepared package exceeded its read buffer"))?,
        );
    }
    if sealed.package_sha256() != <[u8; 32]>::from(digest.finalize()) {
        return Err(invalid("the prepared handoff names another package"));
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid(
            "the prepared TPM handoff is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(invalid("the prepared TPM handoff exceeded its bound"));
    }
    Ok(bytes)
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
                .position(|byte| byte.is_ascii_digit())
                .unwrap();
        edited[digit] = if edited[digit] == b'0' { b'1' } else { b'0' };
        std::fs::write(&sidecar, edited).unwrap();
        assert!(preflight_handoff(&sidecar, &package_path).is_err());
    }
}
