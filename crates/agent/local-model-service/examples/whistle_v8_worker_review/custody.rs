//! Explicit qualification inputs; these hashes are not catalogue authority.
use local_model_service::speech::SpeechError;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use zeroize::Zeroizing;

pub fn verified(path: &str, limit: usize, digest: &str) -> Result<Zeroizing<Vec<u8>>, SpeechError> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(SpeechError::ModelReceipt);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| SpeechError::ModelReceipt)?;
    let metadata = file.metadata().map_err(|_| SpeechError::ModelReceipt)?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
        || metadata.len() == 0
        || metadata.len() > limit as u64
    {
        return Err(SpeechError::ModelReceipt);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| SpeechError::ModelReceipt)?;
    if bytes.len() as u64 != metadata.len() || hex::encode(Sha256::digest(&bytes)) != digest {
        return Err(SpeechError::ModelReceipt);
    }
    Ok(bytes)
}
pub fn sealed(bytes: &[u8]) -> Result<File, SpeechError> {
    // SAFETY: static name and documented flags, error never becomes an owner.
    let descriptor = unsafe {
        libc::memfd_create(
            c"tilo-whistle-review".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if descriptor < 0 {
        return Err(SpeechError::ModelReceipt);
    }
    // SAFETY: successful creation transfers this new descriptor exactly once.
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    file.write_all(bytes)
        .map_err(|_| SpeechError::ModelReceipt)?;
    // SAFETY: private, unmapped descriptor. All four immutable seals are mandatory.
    if unsafe {
        libc::fcntl(
            descriptor,
            libc::F_ADD_SEALS,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
        )
    } != 0
    {
        return Err(SpeechError::ModelReceipt);
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn artifact_refuses_symlink_changed_digest_and_group_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact");
        std::fs::write(&path, b"public fixture").unwrap();
        let digest = hex::encode(Sha256::digest(b"public fixture"));
        assert_eq!(
            &**verified(path.to_str().unwrap(), 64, &digest).unwrap(),
            b"public fixture"
        );
        let alias = dir.path().join("alias");
        symlink(&path, &alias).unwrap();
        assert!(verified(alias.to_str().unwrap(), 64, &digest).is_err());
        assert!(verified(path.to_str().unwrap(), 64, &"0".repeat(64)).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).unwrap();
        assert!(verified(path.to_str().unwrap(), 64, &digest).is_err());
    }

    #[test]
    fn executable_snapshot_is_kernel_immutable_before_loading() {
        let bytes = b"public ABI fixture";
        let mut file = sealed(bytes).unwrap();
        assert!(file.write_all(b"change").is_err());
        assert!(file.set_len(0).is_err());
        // SAFETY: live descriptor; the kernel reports every installed seal.
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        assert_eq!(
            seals,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL
        );
    }
}
