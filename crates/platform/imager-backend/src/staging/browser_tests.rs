//! Exercise the production ciphertext reader, including its no-follow boundary.
use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

struct Receipt {
    bytes: u64,
    digest: [u8; 32],
}
impl Expectation for Receipt {
    fn validate(&self) -> io::Result<()> {
        Ok(())
    }
    fn filename(&self) -> &'static str {
        "browser.sealed"
    }
    fn sealed_bytes(&self) -> u64 {
        self.bytes
    }
    fn ciphertext_sha256(&self) -> [u8; 32] {
        self.digest
    }
}

fn artifact(root: &Path, bytes: &[u8]) -> BrowserArtifact<Receipt> {
    BrowserArtifact {
        source_path: root.join("browser.sealed").to_str().unwrap().into(),
        expectation: Receipt {
            bytes: bytes.len() as u64,
            digest: Sha256::digest(bytes).into(),
        },
    }
}

#[test]
fn reads_only_the_exact_private_ciphertext() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let bytes = b"TILOMIG1ciphertext";
    let artifact = artifact(directory.path(), bytes);
    std::fs::write(&artifact.source_path, bytes).unwrap();
    std::fs::set_permissions(
        &artifact.source_path,
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let owner = rustix::process::geteuid().as_raw();
    assert_eq!(artifact.read(owner).unwrap(), bytes);
    std::fs::write(&artifact.source_path, b"TILOMIG1substitute").unwrap();
    assert!(artifact.read(owner).is_err());
}

#[test]
fn refuses_symlink_and_nonprivate_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let bytes = b"TILOMIG1ciphertext";
    let artifact = artifact(directory.path(), bytes);
    let target = directory.path().join("target");
    std::fs::write(&target, bytes).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&target, &artifact.source_path).unwrap();
    let owner = rustix::process::geteuid().as_raw();
    assert!(artifact.read(owner).is_err());
    std::fs::remove_file(&artifact.source_path).unwrap();
    std::fs::rename(target, &artifact.source_path).unwrap();
    std::fs::set_permissions(
        &artifact.source_path,
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(artifact.read(owner).is_err());
}
