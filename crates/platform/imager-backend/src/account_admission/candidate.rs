//! Temporary root-owned admission in the existing disposable qualification owner.
//! This is artifact scope, never a published consumer capability.
use super::hex_digest;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    io::{self, Read},
    path::Path,
};

const MARKER: &str = "/run/tilo-service-candidate-admission.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    required_protocol: u32,
    browser_package_hash: String,
    cef_runtime_root_hash: String,
    sdk_runtime_root_hash: String,
    receiving_route_qualification_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Admission {
    schema: u32,
    phase: String,
    mount_namespace: u64,
    pid_namespace: u64,
    actor_path: String,
    actor_sha256: String,
    scope: Scope,
    identity_public_key: Option<[u8; 32]>,
    setup_binding: Option<String>,
}
fn refused() -> io::Error {
    io::Error::other("Candidate service admission refused")
}
impl Admission {
    fn check_selection(
        &self,
        required: u32,
        sdk: &str,
        person: Option<(&[u8; 32], &str)>,
    ) -> io::Result<()> {
        if self.schema != 1
            || required != 2
            || self.scope.required_protocol != required
            || sdk != self.scope.sdk_runtime_root_hash
            || ![
                &self.actor_sha256,
                &self.scope.browser_package_hash,
                &self.scope.cef_runtime_root_hash,
                &self.scope.sdk_runtime_root_hash,
                &self.scope.receiving_route_qualification_sha256,
            ]
            .iter()
            .all(|s| hex_digest(s))
        {
            return Err(refused());
        }
        match person {
            None if self.phase == "source"
                && self.identity_public_key.is_none()
                && self.setup_binding.is_none()
                && self.actor_path == "/run/tilo-source-public/host/bin/tilo-imager" =>
            {
                Ok(())
            }
            Some((key, binding))
                if self.phase == "receiver"
                    && self.identity_public_key.as_ref() == Some(key)
                    && self.setup_binding.as_deref() == Some(binding)
                    && self.actor_path == "/usr/libexec/tilo-prepared-setup" =>
            {
                Ok(())
            }
            _ => Err(refused()),
        }
    }
}
fn protected(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    let mut edge = std::path::PathBuf::from("/");
    for component in path.components().skip(1) {
        edge.push(component);
        let info = std::fs::symlink_metadata(&edge)?;
        if info.uid() != 0 || info.mode() & 0o022 != 0 || info.file_type().is_symlink() {
            return Err(refused());
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.nlink() != 1 || before.len() == 0 || before.len() > maximum {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    file.by_ref().take(maximum + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let observed = std::fs::symlink_metadata(path)?;
    if bytes.len() as u64 != before.len()
        || before.dev() != observed.dev()
        || before.ino() != observed.ino()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.mtime() != after.mtime()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || before.len() != after.len()
    {
        return Err(refused());
    }
    Ok(bytes)
}

/// The source door is only for the measured Imager in its owned PAM namespace;
/// the receiving door additionally requires the native decrypted person/binding.
/// Absence is ordinary installed admission, not an environment-controlled mode.
pub(super) fn admits(
    required: u32,
    sdk: &str,
    person: Option<(&[u8; 32], &str)>,
) -> io::Result<bool> {
    if !Path::new(MARKER).try_exists()? {
        return Ok(false);
    }
    let raw = protected(Path::new(MARKER), 16384)?;
    let value: Admission = serde_json::from_slice(&raw).map_err(|_| refused())?;
    value.check_selection(required, sdk, person)?;
    if rustix::process::getuid().as_raw() != 1000
        || rustix::process::geteuid().as_raw() != 1000
        || value.mount_namespace != std::fs::metadata("/proc/self/ns/mnt")?.ino()
        || value.pid_namespace != std::fs::metadata("/proc/self/ns/pid")?.ino()
    {
        return Err(refused());
    }
    // The existing owner holds an initial namespace witness outside this PID
    // namespace. An ordinary installed root marker cannot open this door.
    use std::os::fd::AsRawFd;
    let witness = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open("/run/tilo-service-initial-pidns")?;
    if witness.metadata()?.uid() != 0
        || witness.metadata()?.ino() == value.pid_namespace
        || unsafe { libc::ioctl(witness.as_raw_fd(), 0xb703) } != libc::CLONE_NEWPID
    {
        return Err(refused());
    }
    let executable = std::env::current_exe()?;
    if executable != Path::new(&value.actor_path)
        || hex::encode(Sha256::digest(protected(&executable, 1024 * 1024 * 1024)?))
            != value.actor_sha256
        || protected(Path::new(MARKER), 16384)? != raw
    {
        return Err(refused());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_scope_is_not_an_installed_proof() {
        assert!(
            !Path::new(MARKER).exists(),
            "test requires ordinary installed namespace"
        );
        assert!(!admits(2, &"a".repeat(64), None).unwrap());
    }
    #[test]
    fn receiving_selection_cannot_substitute_person_binding_sdk_or_source_phase() {
        let mut value = Admission {
            schema: 1,
            phase: "receiver".into(),
            mount_namespace: 1,
            pid_namespace: 2,
            actor_path: "/usr/libexec/tilo-prepared-setup".into(),
            actor_sha256: "a".repeat(64),
            scope: Scope {
                required_protocol: 2,
                browser_package_hash: "b".repeat(64),
                cef_runtime_root_hash: "c".repeat(64),
                sdk_runtime_root_hash: "d".repeat(64),
                receiving_route_qualification_sha256: "e".repeat(64),
            },
            identity_public_key: Some([7; 32]),
            setup_binding: Some("native-binding".into()),
        };
        let sdk = "d".repeat(64);
        assert!(
            value
                .check_selection(2, &sdk, Some((&[7; 32], "native-binding")))
                .is_ok()
        );
        assert!(
            value
                .check_selection(2, &sdk, Some((&[8; 32], "native-binding")))
                .is_err()
        );
        assert!(
            value
                .check_selection(2, &sdk, Some((&[7; 32], "another-binding")))
                .is_err()
        );
        assert!(
            value
                .check_selection(2, &"f".repeat(64), Some((&[7; 32], "native-binding")))
                .is_err()
        );
        assert!(
            value
                .check_selection(1, &sdk, Some((&[7; 32], "native-binding")))
                .is_err()
        );
        assert!(value.check_selection(2, &sdk, None).is_err());
        value.phase = "source".into();
        assert!(
            value
                .check_selection(2, &sdk, Some((&[7; 32], "native-binding")))
                .is_err()
        );
    }
    #[test]
    fn scope_rejects_receipt_flags_and_unknown_fields() {
        assert!(
            serde_json::from_value::<Scope>(serde_json::json!({
                "required_protocol": 2, "ready": true
            }))
            .is_err()
        );
    }
}
