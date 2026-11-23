//! Selected owner Slack OSCrypt, bound to exact public application bytes.
//! Public package provenance: see CONTRACT.md in the outside proposal.
use super::{
    ChromiumDecryptor, SourceError, SourceGrant,
    chromium_linux::{ChromiumBrand, ChromiumLinux},
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use zeroize::Zeroizing;

/// Held public application proof; never an account resource or caller DTO.
pub struct SlackApplication {
    files: Vec<super::slack_locator::PublicFile>,
}
impl std::fmt::Debug for SlackApplication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SlackApplication { verified public application }")
    }
}
impl SlackApplication {
    pub fn at(root: &std::path::Path) -> Result<Arc<Self>, SourceError> {
        use super::slack_locator::PublicFile;
        let executable = PublicFile::open(&root.join("slack"))?;
        verify_hash(
            &executable,
            228_605_256,
            "cb313a0f37c87aba8669cb041631b8679284ee19c7eaeff201556e7e240a4cc3",
        )?;
        let application = PublicFile::open(&root.join("resources/app.asar"))?;
        // Repacked Flatpak archives must preserve every pinned entry; only the
        // official desktopName patch is permitted. Native bytes satisfy this too.
        let length = application
            .file
            .metadata()
            .map_err(super::unavailable)?
            .len();
        if length > 16 * 1024 * 1024 {
            return Err(SourceError::UnsupportedEncryption);
        }
        use std::os::unix::fs::FileExt;
        let mut bytes = vec![0; usize::try_from(length).map_err(|_| SourceError::Unavailable)?];
        application
            .file
            .read_exact_at(&mut bytes, 0)
            .map_err(super::unavailable)?;
        let unpacked = super::slack_asar::verify(&bytes)?;
        application.validate()?;
        let mut files = vec![executable, application];
        if unpacked.len() != super::slack_modules::MODULES.len()
            || !unpacked
                .iter()
                .zip(super::slack_modules::MODULES)
                .all(|((path, _, _), (actual, _, _))| path.as_str() == *actual)
        {
            return Err(SourceError::UnsupportedEncryption);
        }
        // Slack's public packaging strips some modules after producing ASAR.
        // Preserve the exact pinned header, but require actual distribution bytes.
        for (path, size, sha) in super::slack_modules::MODULES {
            let file = PublicFile::open(&root.join("resources/app.asar.unpacked").join(path))?;
            verify_hash(&file, *size, sha)?;
            files.push(file);
        }
        let proof = Arc::new(Self { files });
        proof.validate()?;
        Ok(proof)
    }
    pub fn validate(&self) -> Result<(), SourceError> {
        for file in &self.files {
            file.validate()?;
        }
        Ok(())
    }
}
fn verify_hash(
    file: &super::slack_locator::PublicFile,
    size: u64,
    sha: &str,
) -> Result<(), SourceError> {
    use std::os::unix::fs::FileExt;
    file.validate()?;
    if file.file.metadata().map_err(super::unavailable)?.len() != size || size > 256 * 1024 * 1024 {
        return Err(SourceError::UnsupportedEncryption);
    }
    let mut digest = Sha256::new();
    let mut offset = 0;
    let mut buffer = [0; 64 * 1024];
    while offset < size {
        let limit = usize::try_from((size - offset).min(buffer.len() as u64))
            .map_err(|_| SourceError::Unavailable)?;
        let read = file
            .file
            .read_at(&mut buffer[..limit], offset)
            .map_err(super::unavailable)?;
        if read == 0 {
            return Err(SourceError::UnsupportedEncryption);
        }
        digest.update(&buffer[..read]);
        offset += read as u64;
    }
    file.validate()?;
    if hex::encode(digest.finalize()) != sha {
        return Err(SourceError::UnsupportedEncryption);
    }
    Ok(())
}

pub struct SlackLinux {
    application: Arc<SlackApplication>,
    inner: ChromiumLinux,
}
impl SlackLinux {
    pub fn for_grant(
        grant: &SourceGrant,
        application: Arc<SlackApplication>,
    ) -> Result<Self, SourceError> {
        application.validate()?;
        let inner = ChromiumLinux::for_application(grant, ChromiumBrand::Slack452178)?;
        application.validate()?;
        Ok(Self { application, inner })
    }
}
impl ChromiumDecryptor for SlackLinux {
    fn approved_profile(&self) -> (u32, u64, u64) {
        self.inner.approved_profile()
    }
    fn decrypt(
        &self,
        host: &str,
        schema: i64,
        encrypted: &[u8],
    ) -> Result<Zeroizing<String>, SourceError> {
        self.application.validate()?;
        let result = self.inner.decrypt(host, schema, encrypted);
        self.application.validate()?;
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    #[test]
    fn changed_or_unpinned_public_application_never_mints_slack_key_authority() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("resources")).unwrap();
        let file = File::create(directory.path().join("slack")).unwrap();
        file.set_len(228_605_256).unwrap();
        assert!(SlackApplication::at(directory.path()).is_err());
        std::fs::write(directory.path().join("resources/app.asar"), b"unreviewed").unwrap();
        assert!(SlackApplication::at(directory.path()).is_err());
    }
}
