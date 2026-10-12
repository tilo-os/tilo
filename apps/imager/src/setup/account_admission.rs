//! Admission of protected account custody by the selected image and actual
//! installed consumers. A staged importer never certifies its own target.
use serde::{Deserialize, Serialize};
use std::io;

/// Image-produced proof, carried inside the authenticated component manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerCapability {
    pub schema: u32,
    pub protected_grant_protocol: u32,
    pub credential_helper_sha256: String,
    pub runtime_root_hash: String,
    pub runtime_probe_sha256: String,
    pub runtime_library_sha256: String,
    pub runtime_version: String,
    pub runtime_library: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_sessions: Option<ServiceConsumerCapability>,
}
/// Actual receiving Browser and native CEF qualification bound to the SDK proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConsumerCapability {
    pub schema: u32,
    pub protocol: u32,
    pub sdk_runtime_root_hash: String,
    pub browser_package_hash: String,
    pub browser_publisher: String,
    pub browser_binary_sha256: String,
    pub cef_runtime_version: String,
    pub cef_runtime_root_hash: String,
    pub cef_runtime_publisher: String,
    pub cef_host_sha256: String,
    pub qualification_script_sha256: String,
    pub receiving_shell_path: String,
    pub receiving_shell_sha256: String,
    pub receiving_route_qualification_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_storage_qualification_sha256: Option<String>,
}
impl ServiceConsumerCapability {
    fn validate(&self, sdk_root: &str) -> io::Result<()> {
        let hashes = [
            &self.sdk_runtime_root_hash,
            &self.browser_package_hash,
            &self.browser_publisher,
            &self.browser_binary_sha256,
            &self.cef_runtime_root_hash,
            &self.cef_runtime_publisher,
            &self.cef_host_sha256,
            &self.qualification_script_sha256,
            &self.receiving_shell_sha256,
            &self.receiving_route_qualification_sha256,
        ];
        if self.schema != 1
            || !matches!(self.protocol, 1 | 2)
            || self.sdk_runtime_root_hash != sdk_root
            || self.receiving_shell_sha256 != self.receiving_route_qualification_sha256
            || !matches!(
                self.receiving_shell_path.as_str(),
                "/usr/bin/shell" | "/usr/bin/tilo-shell"
            )
            || (self.protocol == 2 && self.origin_storage_qualification_sha256.is_none())
            || self
                .origin_storage_qualification_sha256
                .as_ref()
                .is_some_and(|value| {
                    self.protocol != 2
                        || value != &self.qualification_script_sha256
                        || value.len() != 64
                        || !value
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            || hashes.iter().any(|value| {
                value.len() != 64
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            || self.cef_runtime_version.is_empty()
            || self.cef_runtime_version.len() > 256
            || !self
                .cef_runtime_version
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !self
                .cef_runtime_version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Service consumer proof is invalid",
            ));
        }
        Ok(())
    }
}

impl ConsumerCapability {
    /// Validate the bounded image receipt; authentication is the caller's job.
    /// # Errors
    /// Rejects unknown protocols, malformed hashes and noncanonical runtime paths.
    pub fn validate(&self) -> io::Result<()> {
        let hex = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if self.schema != 1
            || self.protected_grant_protocol != 1
            || !hex(&self.credential_helper_sha256)
            || !hex(&self.runtime_root_hash)
            || !hex(&self.runtime_probe_sha256)
            || !hex(&self.runtime_library_sha256)
            || self.runtime_version.is_empty()
            || self.runtime_version.len() > 256
            || !self
                .runtime_version
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !self
                .runtime_version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
            || self.runtime_library != "lib/libruntime_dylib.so"
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Account consumer proof is invalid",
            ));
        }
        if let Some(service) = &self.service_sessions {
            service.validate(&self.runtime_root_hash)?;
        }
        Ok(())
    }

    /// True only for a validated, SDK-bound receiving Browser/CEF proof.
    pub fn supports_service_sessions(&self) -> bool {
        self.supports_service_session_protocol(1)
    }

    /// Admission for the exact authenticated artifact's minimum protocol.
    /// Protocol 1 carries cookies; protocol 2 also carries durable origin state.
    pub fn supports_service_session_protocol(&self, required: u32) -> bool {
        matches!(required, 1 | 2)
            && self.validate().is_ok()
            && self
                .service_sessions
                .as_ref()
                .is_some_and(|proof| proof.protocol >= required)
    }
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
    const VERIFIER: &str = "/usr/libexec/tilo-verify-account-consumer-capability";
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
    let proof: ConsumerCapability = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    proof.validate()?;
    Ok(proof)
}
#[cfg(target_os = "linux")]
fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "The installed system cannot use the selected saved connections",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_admission_requires_nested_receiving_artifacts_bound_to_selected_sdk() {
        let legacy = serde_json::json!({
            "schema":1,"protected_grant_protocol":1,
            "credential_helper_sha256":"1".repeat(64),"runtime_root_hash":"2".repeat(64),
            "runtime_probe_sha256":"3".repeat(64),"runtime_library_sha256":"4".repeat(64),
            "runtime_version":"2026.10","runtime_library":"lib/libruntime_dylib.so"
        });
        let mut proof: ConsumerCapability = serde_json::from_value(legacy).unwrap();
        proof.validate().unwrap();
        assert!(!proof.supports_service_sessions());
        proof.service_sessions = Some(ServiceConsumerCapability {
            schema: 1,
            protocol: 1,
            sdk_runtime_root_hash: proof.runtime_root_hash.clone(),
            browser_package_hash: "5".repeat(64),
            browser_publisher: "6".repeat(64),
            browser_binary_sha256: "7".repeat(64),
            cef_runtime_version: "2026.10-chromium".into(),
            cef_runtime_root_hash: "8".repeat(64),
            cef_runtime_publisher: "9".repeat(64),
            cef_host_sha256: "a".repeat(64),
            qualification_script_sha256: "b".repeat(64),
            receiving_shell_path: "/usr/bin/shell".into(),
            receiving_shell_sha256: "c".repeat(64),
            receiving_route_qualification_sha256: "c".repeat(64),
            origin_storage_qualification_sha256: None,
        });
        assert!(proof.supports_service_sessions());
        assert!(!proof.supports_service_session_protocol(2));
        let valid = proof.clone();
        proof.service_sessions.as_mut().unwrap().protocol = 2;
        assert!(!proof.supports_service_session_protocol(2));
        proof
            .service_sessions
            .as_mut()
            .unwrap()
            .origin_storage_qualification_sha256 = Some("b".repeat(64));
        assert!(proof.supports_service_session_protocol(2));
        proof
            .service_sessions
            .as_mut()
            .unwrap()
            .origin_storage_qualification_sha256 = Some("e".repeat(64));
        assert!(!proof.supports_service_session_protocol(2));
        assert!(proof.validate().is_err());
        proof
            .service_sessions
            .as_mut()
            .unwrap()
            .origin_storage_qualification_sha256 = Some("b".repeat(64));
        assert!(proof.supports_service_session_protocol(1));
        assert!(!proof.supports_service_session_protocol(3));
        proof = valid.clone();
        proof
            .service_sessions
            .as_mut()
            .unwrap()
            .sdk_runtime_root_hash = "c".repeat(64);
        assert!(!proof.supports_service_sessions());
        proof = valid.clone();
        proof
            .service_sessions
            .as_mut()
            .unwrap()
            .cef_host_sha256
            .clear();
        assert!(!proof.supports_service_sessions());
        proof = valid.clone();
        proof
            .service_sessions
            .as_mut()
            .unwrap()
            .receiving_shell_path = "/tmp/shell".into();
        assert!(!proof.supports_service_sessions());
        proof = valid.clone();
        proof
            .service_sessions
            .as_mut()
            .unwrap()
            .receiving_route_qualification_sha256
            .clear();
        assert!(!proof.supports_service_sessions());
        proof = valid.clone();
        proof
            .service_sessions
            .as_mut()
            .unwrap()
            .receiving_route_qualification_sha256 = "d".repeat(64);
        assert!(!proof.supports_service_sessions());
        assert!(proof.validate().is_err());
        proof = valid;
        proof.service_sessions.as_mut().unwrap().protocol = 0;
        assert!(!proof.supports_service_sessions());
    }
}
