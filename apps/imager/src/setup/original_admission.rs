//! App image review over backend-owned consumer-proof envelopes.
pub use imager_backend::original_admission::{
    CapturePins, ConsumerCapability, ORIGINAL_SOURCE_PROTOCOL, PROOF_BOUND, RuntimeHelper, require,
};
use std::io;

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Original-source consumer proof syntax is invalid",
    )
}
/// Device-local producer review. This is never serialized into a helper request
/// or constructed from an app action's JSON. The private declaration preserves
/// the selected authenticated release proof until its exact image is held.
#[derive(Clone, Debug)]
#[cfg(target_os = "linux")]
pub(crate) struct ReviewedOriginalImage {
    source: crate::model::Source,
    proof: ConsumerCapability,
    applications: Option<super::applications_admission::ConsumerCapability>,
}
#[cfg(target_os = "linux")]
impl ReviewedOriginalImage {
    pub(crate) fn review(
        source: &crate::model::Source,
        releases: &[crate::model::TiloRelease],
    ) -> io::Result<Self> {
        let mut matches = releases.iter().filter(|release| {
            release.matches_source(source)
                && source.size == release.size
                && source.expected_sha256.as_deref() == Some(release.image_sha256.as_str())
        });
        let release = matches.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "Original-source installer image is unavailable",
            )
        })?;
        if matches.next().is_some() || !source.installable || source.identity.is_none() {
            return Err(invalid());
        }
        let proof = release.original_source.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "Selected image has no original-source consumer proof",
            )
        })?;
        proof.validate()?;
        if proof.capture_pins.arch != release.architecture {
            return Err(invalid());
        }
        Ok(Self {
            source: source.clone(),
            proof: proof.clone(),
            applications: release.applications.clone(),
        })
    }
    pub(crate) fn require_protocol(&self, protocol: u32) -> io::Result<()> {
        require(Some(&self.proof), Some(protocol))
    }
    pub(crate) fn require_applications(&self) -> io::Result<()> {
        self.require_protocol(6)?;
        let proof = self.applications.as_ref();
        super::applications_admission::require(proof, Some(2))
    }
    pub(crate) fn source(&self) -> &crate::model::Source {
        &self.source
    }
    pub(crate) fn open_verified(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<VerifiedOriginalImage> {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
        self.proof.validate()?;
        super::migration::check_cancel(cancel)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&self.source.path)?;
        let before = file.metadata()?;
        if !before.is_file()
            || before.len() != self.source.size
            || crate::model::SourceIdentity::from_file(&file)? != self.source.identity
        {
            return Err(invalid());
        }
        let mut hash = Sha256::new();
        let mut offset = 0_u64;
        let mut buffer = [0_u8; 128 * 1024];
        loop {
            super::migration::check_cancel(cancel)?;
            let count = file.read_at(&mut buffer, offset)?;
            if count == 0 {
                break;
            }
            offset = offset.checked_add(count as u64).ok_or_else(invalid)?;
            if offset > self.source.size {
                return Err(invalid());
            }
            hash.update(&buffer[..count]);
        }
        let after = file.metadata()?;
        if offset != self.source.size
            || self.source.expected_sha256.as_deref() != Some(hex::encode(hash.finalize()).as_str())
            || (
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
            )
        {
            return Err(invalid());
        }
        super::migration::check_cancel(cancel)?;
        let stamp = ImageStamp::from_metadata(&after);
        let verified = VerifiedOriginalImage {
            file,
            reviewed: self.clone(),
            stamp,
        };
        verified.validate()?;
        Ok(verified)
    }
}

#[cfg(target_os = "linux")]
#[derive(PartialEq, Eq)]
struct ImageStamp {
    device: u64,
    inode: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
#[cfg(target_os = "linux")]
impl ImageStamp {
    fn read(file: &std::fs::File) -> io::Result<Self> {
        let m = file.metadata()?;
        Ok(Self::from_metadata(&m))
    }
    fn from_metadata(m: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: m.dev(),
            inode: m.ino(),
            bytes: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
}
/// Retains the actual digest-checked inode and its immutable observation through
/// source permission and pending setup preparation. A bare File is not proof.
#[cfg(target_os = "linux")]
pub(crate) struct VerifiedOriginalImage {
    file: std::fs::File,
    reviewed: ReviewedOriginalImage,
    stamp: ImageStamp,
}
#[cfg(target_os = "linux")]
impl VerifiedOriginalImage {
    pub(crate) fn validate(&self) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        self.reviewed.proof.validate()?;
        if ImageStamp::read(&self.file)? != self.stamp {
            return Err(invalid());
        }
        let named = std::fs::symlink_metadata(&self.reviewed.source.path)?;
        if !named.is_file() || named.dev() != self.stamp.device || named.ino() != self.stamp.inode {
            return Err(invalid());
        }
        Ok(())
    }
    pub(crate) fn clone_descriptor(&self) -> io::Result<std::fs::File> {
        self.validate()?;
        let clone = self.file.try_clone()?;
        self.validate()?;
        Ok(clone)
    }
}

#[cfg(test)]
pub(crate) fn syntax_fixture() -> ConsumerCapability {
    let hex = "a".repeat(64);
    ConsumerCapability {
        schema: 1,
        original_source_protocol: 5,
        prepared_helper_sha256: hex.clone(),
        installed_launcher_sha256: hex.clone(),
        runtime_launcher_sha256: hex.clone(),
        runtime_sdk_sha256: hex.clone(),
        runtime_tar_sha256: hex.clone(),
        runtime_openpgp_table_sha256: hex.clone(),
        qualifier_receipt_sha256: hex.clone(),
        capture_pins: CapturePins {
            schema: 1,
            worker_protocol: 5,
            arch: "x86_64".into(),
            package_id: "so.tilo.imager.home-export".into(),
            service_publisher: hex.clone(),
            package_hash: hex.clone(),
            manifest_sha256: hex.clone(),
            payload_root_hash: hex.clone(),
            runtime_version: "1.0".into(),
            runtime_publisher: hex.clone(),
            runtime_root_hash: hex.clone(),
        },
        runtime_helpers: [
            ("agent", "bin/gpg-agent"),
            ("gpgconf", "bin/gpgconf"),
            ("openpgp", "bin/gpg"),
            ("ostree", "bin/ostree"),
        ]
        .into_iter()
        .map(|(role, bundle)| RuntimeHelper {
            role: role.into(),
            bundle: bundle.into(),
            sha256: hex.clone(),
        })
        .collect(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn original_image_review_refuses_legacy_ambiguous_and_changed_held_images() {
        use sha2::{Digest, Sha256};
        use std::sync::atomic::AtomicBool;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("selected-image");
        std::fs::write(&path, b"reviewed image bytes").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mut release = crate::model::sample_releases().remove(0);
        release.size = file.metadata().unwrap().len();
        release.image_sha256 = hex::encode(Sha256::digest(b"reviewed image bytes"));
        let mut source = release.to_source();
        source.path = path.to_string_lossy().into_owned();
        source.identity = crate::model::SourceIdentity::from_file(&file).unwrap();
        assert_eq!(
            ReviewedOriginalImage::review(&source, &[release.clone()])
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        let mut proof = syntax_fixture();
        proof.capture_pins.arch = release.architecture.clone();
        release.original_source = Some(proof);
        let reviewed = ReviewedOriginalImage::review(&source, &[release.clone()]).unwrap();
        let held = reviewed.open_verified(&AtomicBool::new(false)).unwrap();
        assert_eq!(
            held.clone_descriptor().unwrap().metadata().unwrap().len(),
            source.size
        );
        assert!(ReviewedOriginalImage::review(&source, &[release.clone(), release]).is_err());
        // Same path, inode and length still pass identity-only review; the
        // authenticated selected image digest must refuse changed bytes.
        std::fs::write(&path, b"tampered image bytes").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), source.size);
        assert!(held.validate().is_err());
        assert!(held.clone_descriptor().is_err());
        assert!(reviewed.open_verified(&AtomicBool::new(false)).is_err());
        assert!(reviewed.open_verified(&AtomicBool::new(true)).is_err());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(directory.path().join("replacement"), &path).unwrap();
        std::fs::write(
            directory.path().join("replacement"),
            b"reviewed image bytes",
        )
        .unwrap();
        assert!(reviewed.open_verified(&AtomicBool::new(false)).is_err());
    }
}
