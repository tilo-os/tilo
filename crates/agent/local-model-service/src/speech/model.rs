//! Immutable receipt snapshots for native readers; no path access after confinement.
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};

use super::SpeechError;
use sha2::{Digest as _, Sha256};

const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

/// A privately constructed, digest-checked immutable descriptor. No caller can
/// manufacture this witness from a mutable installed path or arbitrary bytes.
pub struct SealedSpeechModel {
    file: File,
    size: u64,
    id: String,
    revision: String,
    format: agent_provider_config::local::ModelArtifactFormat,
}
impl SealedSpeechModel {
    pub fn format(&self) -> agent_provider_config::local::ModelArtifactFormat {
        self.format
    }
    pub(super) fn with_format(
        mut self,
        format: agent_provider_config::local::ModelArtifactFormat,
    ) -> Self {
        self.format = format;
        self
    }
    pub fn file(&self) -> &File {
        &self.file
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub(super) fn check_seals(&self) -> Result<(), SpeechError> {
        // SAFETY: this owner holds the live descriptor; querying seals mutates nothing.
        let seals = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0 || seals & SEALS != SEALS {
            return Err(SpeechError::ModelReceipt);
        }
        Ok(())
    }
}

pub(super) fn seal_verified(
    source: File,
    size: u64,
    expected: &str,
    id: String,
    revision: String,
) -> Result<SealedSpeechModel, SpeechError> {
    if size > super::MAX_MODEL_BYTES {
        return Err(SpeechError::ModelUnavailable);
    }
    // SAFETY: static NUL name and documented flags; errors never become an owned fd.
    let descriptor = unsafe {
        libc::memfd_create(
            c"tilo-speech-model".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if descriptor < 0 {
        return Err(SpeechError::ModelUnavailable);
    }
    // SAFETY: successful memfd_create transfers this new descriptor to exactly one owner.
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    let mut source = source.take(size.checked_add(1).ok_or(SpeechError::ModelReceipt)?);
    let mut buffer = zeroize::Zeroizing::new(vec![0_u8; 32 * 1024]);
    let mut digest = Sha256::new();
    let mut copied = 0_u64;
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(|_| SpeechError::ModelReceipt)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(u64::try_from(read).map_err(|_| SpeechError::ModelReceipt)?)
            .ok_or(SpeechError::ModelReceipt)?;
        if copied > size {
            return Err(SpeechError::ModelReceipt);
        }
        let chunk = buffer.get(..read).ok_or(SpeechError::ModelReceipt)?;
        digest.update(chunk);
        file.write_all(chunk)
            .map_err(|_| SpeechError::ModelUnavailable)?;
    }
    if copied != size || hex::encode(digest.finalize()) != expected {
        return Err(SpeechError::ModelReceipt);
    }
    // SAFETY: privately owned, unmapped descriptor; all four seals are mandatory.
    if unsafe { libc::fcntl(descriptor, libc::F_ADD_SEALS, SEALS) } != 0 {
        return Err(SpeechError::ModelReceipt);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| SpeechError::ModelReceipt)?;
    let model = SealedSpeechModel {
        file,
        size,
        id,
        revision,
        format: agent_provider_config::local::ModelArtifactFormat::WhisperGgml,
    };
    model.check_seals()?;
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_descriptor_stays_immutable_after_source_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source");
        let bytes = b"immutable verified model fixture";
        std::fs::write(&path, bytes).unwrap();
        let model = seal_verified(
            File::open(&path).unwrap(),
            bytes.len() as u64,
            &hex::encode(Sha256::digest(bytes)),
            "test".into(),
            "revision".into(),
        )
        .unwrap();
        std::fs::write(&path, b"different source bytes").unwrap();
        let mut actual = Vec::new();
        model.file().read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes);
        assert!(model.file().set_len(0).is_err());
        assert!(model.file().write_all(b"x").is_err());
        // SAFETY: live owned descriptor; sealing forbids removing or adding seals.
        assert_eq!(
            unsafe {
                libc::fcntl(
                    model.file().as_raw_fd(),
                    libc::F_ADD_SEALS,
                    libc::F_SEAL_FUTURE_WRITE,
                )
            },
            -1
        );
        model.check_seals().unwrap();
    }

    #[test]
    fn snapshot_refuses_changed_size_digest_and_existing_admission_limit() {
        let file = tempfile::tempfile().unwrap();
        (&file).write_all(b"fixture").unwrap();
        for (size, digest) in [
            (6, hex::encode(Sha256::digest(b"fixtur"))),
            (8, hex::encode(Sha256::digest(b"fixturex"))),
            (7, "0".repeat(64)),
        ] {
            let mut source = file.try_clone().unwrap();
            source.seek(SeekFrom::Start(0)).unwrap();
            assert!(
                seal_verified(source, size, &digest, "test".into(), "revision".into()).is_err()
            );
        }
        assert!(matches!(
            seal_verified(
                file,
                742_090_464,
                &"0".repeat(64),
                "test".into(),
                "revision".into()
            ),
            Err(SpeechError::ModelUnavailable)
        ));
    }
}
