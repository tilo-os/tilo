//! Admission of Home archives by authenticated image-produced consumer proofs.
//! A source-side exporter never certifies the target importer.
use serde::{Deserialize, Serialize};
use std::io;

pub const ARCHIVE_PROTOCOL: u32 = 2;
pub const SEED_PROTOCOL: u32 = 1;

/// Exact installed consumers, carried by the signed image catalog or manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerCapability {
    pub schema: u32,
    pub archive_protocol: u32,
    pub seed_protocol: u32,
    pub firstboot_user_sha256: String,
    pub seed_helper_sha256: String,
    pub prepared_helper_sha256: String,
    pub tar_sha256: String,
}
impl ConsumerCapability {
    /// Check bounded proof syntax; authentication belongs to the image selection.
    /// # Errors
    /// Refuses unsupported protocols and malformed consumer digests.
    pub fn validate(&self) -> io::Result<()> {
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if self.schema != 1
            || self.archive_protocol != ARCHIVE_PROTOCOL
            || self.seed_protocol != SEED_PROTOCOL
            || !digest(&self.firstboot_user_sha256)
            || !digest(&self.seed_helper_sha256)
            || !digest(&self.prepared_helper_sha256)
            || !digest(&self.tar_sha256)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Home consumer proof is invalid",
            ));
        }
        Ok(())
    }
}

/// Refuse Home handoff without the selected image's authenticated consumer proof.
/// No Home declaration preserves the ordinary fresh-install path.
/// # Errors
/// Rejects absent, malformed, or incompatible target proofs.
pub fn require(capability: Option<&ConsumerCapability>, required: Option<u32>) -> io::Result<()> {
    if let Some(protocol) = required {
        let proof = capability.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "The selected system cannot import the Home archive",
            )
        })?;
        proof.validate()?;
        if proof.archive_protocol != protocol {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "The selected system cannot import the Home archive",
            ));
        }
    }
    Ok(())
}

/// Authenticate the actual installed consumer proof before target custody writes.
/// # Errors
/// Refuses old/missing consumers, an untrusted verifier, or failed native checks.
#[cfg(target_os = "linux")]
pub fn installed() -> io::Result<ConsumerCapability> {
    use std::{
        fs::OpenOptions,
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
        process::{Command, Stdio},
    };
    const VERIFIER: &str = "/usr/libexec/tilo-verify-home-consumer-capability";
    // Every namespace edge is root-controlled; a target user cannot replace
    // the verifier with their importer or another executable between checks.
    for path in ["/", "/usr", "/usr/libexec", VERIFIER] {
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 || metadata.file_type().is_symlink()
        {
            return Err(unavailable());
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(VERIFIER)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.mode() & 0o111 == 0 || metadata.len() > 1024 * 1024 {
        return Err(unavailable());
    }
    let mut child = Command::new(VERIFIER)
        .arg("--check")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().ok_or_else(unavailable)?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let _reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(4097).read_to_end(&mut bytes).map(|_| bytes);
        let _ = send.send(result);
    });
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(150))
        .ok_or_else(unavailable)?;
    let result = receive.recv_timeout(std::time::Duration::from_secs(150));
    let bytes = match result {
        Ok(Ok(bytes)) if bytes.len() <= 4096 => bytes,
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable());
        }
    };
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(unavailable());
            }
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Retain the original verifier handle until all target checks finish.
    let observed = std::fs::symlink_metadata(VERIFIER)?;
    if observed.dev() != metadata.dev()
        || observed.ino() != metadata.ino()
        || file.metadata()?.ino() != metadata.ino()
    {
        return Err(unavailable());
    }
    let shape: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    if !shape.is_object() {
        return Err(unavailable());
    }
    let proof: ConsumerCapability = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    proof.validate()?;
    Ok(proof)
}
#[cfg(target_os = "linux")]
fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "The installed system cannot use the selected Home archive",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn proof() -> ConsumerCapability {
        ConsumerCapability {
            schema: 1,
            archive_protocol: ARCHIVE_PROTOCOL,
            seed_protocol: SEED_PROTOCOL,
            firstboot_user_sha256: "c".repeat(64),
            seed_helper_sha256: "d".repeat(64),
            prepared_helper_sha256: "a".repeat(64),
            tar_sha256: "b".repeat(64),
        }
    }
    #[test]
    fn home_declaration_requires_independent_protocol_two_consumers() {
        assert!(require(None, None).is_ok());
        assert!(require(None, Some(ARCHIVE_PROTOCOL)).is_err());
        assert!(require(Some(&proof()), Some(ARCHIVE_PROTOCOL)).is_ok());
        assert!(require(Some(&proof()), Some(1)).is_err());
        for digest in ["A".repeat(64), "f".repeat(63), "g".repeat(64)] {
            let mut malformed = proof();
            malformed.tar_sha256 = digest;
            assert!(require(Some(&malformed), Some(ARCHIVE_PROTOCOL)).is_err());
        }
        let mut incompatible_seed = proof();
        incompatible_seed.seed_protocol = 2;
        assert!(require(Some(&incompatible_seed), Some(ARCHIVE_PROTOCOL)).is_err());
        let mut substituted_seed = proof();
        substituted_seed.seed_helper_sha256 = "D".repeat(64);
        assert!(require(Some(&substituted_seed), Some(ARCHIVE_PROTOCOL)).is_err());
        let mut substituted_firstboot = proof();
        substituted_firstboot.firstboot_user_sha256 = String::new();
        assert!(require(Some(&substituted_firstboot), Some(ARCHIVE_PROTOCOL)).is_err());
        let mut unsupported = proof();
        unsupported.archive_protocol = 1;
        assert!(require(Some(&unsupported), Some(ARCHIVE_PROTOCOL)).is_err());
    }
}
