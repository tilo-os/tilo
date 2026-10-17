//! Admission of protected account custody by the selected image and actual
//! installed consumers. A staged importer never certifies its own target.
#[cfg(target_os = "linux")]
mod candidate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io};

/// The fixed measured Python receiving closure of an installed service receiver.
pub const RECEIVING_DEPENDENCIES: &[&str] = &[
    "/usr/libexec/tilo-installer-receive-fixture/receive.py",
    "/usr/libexec/tilo-installer-receive-fixture/semantic.py",
    "/usr/libexec/tilo-installer-receive-fixture/firstuse.py",
    "/usr/libexec/tilo-installer-receive-fixture/delivery.py",
    "/usr/libexec/tilo-installer-receive-fixture/namespace.py",
    "/usr/libexec/tilo-installer-receive-fixture/packages.py",
    "/usr/libexec/tilo-installer-receive-fixture/apps.py",
    "/usr/libexec/tilo-installer-receive-fixture/cloud.py",
    "/usr/libexec/tilo-installer-receive-fixture/service_sessions.py",
    "/usr/libexec/tilo-installer-receive-fixture/service_contract.py",
    "/usr/libexec/tilo-installer-receive-fixture/service_processes.py",
    "/usr/libexec/tilo-installer-source-fixture/source.py",
    "/usr/libexec/tilo-installer-source-fixture/owned_gpt.py",
    "/usr/libexec/tilo-installer-source-fixture/bounded_native.py",
    "/usr/libexec/tilo-installer-source-fixture/flatpak_material.py",
    "/usr/libexec/tilo-installer-source-fixture/source_sessions.py",
];

fn hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn receiving_dependencies<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("unique measured dependency paths")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate receiving dependency"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique)
}

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
    pub receiving_entry_sha256: String,
    #[serde(deserialize_with = "receiving_dependencies")]
    pub receiving_dependencies: BTreeMap<String, String>,
    pub receiving_python_path: String,
    pub receiving_python_sha256: String,
    pub receiving_route_qualification_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_storage_qualification_sha256: Option<String>,
}
impl ServiceConsumerCapability {
    // Python uses sort_keys=True and compact JSON. All admitted keys/values
    // here are ASCII, and BTreeMap preserves the same lexical key ordering.
    fn measured_closure_hash(&self) -> io::Result<String> {
        #[derive(Serialize)]
        struct Closure<'a> {
            receiving_dependencies: &'a BTreeMap<String, String>,
            receiving_entry_sha256: &'a str,
            receiving_python_path: &'a str,
            receiving_python_sha256: &'a str,
            receiving_shell_sha256: &'a str,
        }
        let encoded = serde_json::to_vec(&Closure {
            receiving_dependencies: &self.receiving_dependencies,
            receiving_entry_sha256: &self.receiving_entry_sha256,
            receiving_python_path: &self.receiving_python_path,
            receiving_python_sha256: &self.receiving_python_sha256,
            receiving_shell_sha256: &self.receiving_shell_sha256,
        })?;
        let mut hash = Sha256::new();
        hash.update(b"tilo-installed-service-receiver-python-v1\0");
        hash.update(encoded);
        Ok(hex::encode(hash.finalize()))
    }

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
            &self.receiving_entry_sha256,
            &self.receiving_python_sha256,
            &self.receiving_route_qualification_sha256,
        ];
        if self.schema != 2
            || !matches!(self.protocol, 1 | 2)
            || self.sdk_runtime_root_hash != sdk_root
            || self.receiving_dependencies.len() != RECEIVING_DEPENDENCIES.len()
            || RECEIVING_DEPENDENCIES.iter().any(|path| {
                !self
                    .receiving_dependencies
                    .get(*path)
                    .is_some_and(|hash| hex_digest(hash))
            })
            || self.receiving_dependencies.get(RECEIVING_DEPENDENCIES[0])
                != Some(&self.receiving_entry_sha256)
            || !self
                .receiving_python_path
                .strip_prefix("/usr/bin/python3")
                .is_some_and(|suffix| {
                    suffix.is_empty()
                        || suffix.strip_prefix('.').is_some_and(|version| {
                            !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())
                        })
                })
            || self.receiving_route_qualification_sha256 != self.measured_closure_hash()?
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

/// Candidate-scoped source admission; never changes installed proof semantics.
///
/// # Errors
/// Refuses invalid consumer proofs or an unauthenticated candidate namespace.
pub fn source_candidate(required: u32, proof: &ConsumerCapability) -> io::Result<bool> {
    proof.validate()?;
    #[cfg(target_os = "linux")]
    {
        candidate::admits(required, &proof.runtime_root_hash, None)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = required;
        Ok(false)
    }
}

/// Receiving admission binds the native authenticated sealed manifest's person.
///
/// # Errors
/// Refuses candidate markers not bound to the measured actor and selected person.
pub fn receiving_candidate(
    required: u32,
    sdk: &str,
    person: &[u8; 32],
    binding: &str,
) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        candidate::admits(required, sdk, Some((person, binding)))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (required, sdk, person, binding);
        Ok(false)
    }
}

/// Authenticate the actual installed consumer proof before target custody writes.
/// The check re-executes the installed probes and compares the image receipt.
/// # Errors
/// Refuses old/missing consumers, untrusted artifacts, or failed native checks.
#[cfg(target_os = "linux")]
pub fn installed() -> io::Result<ConsumerCapability> {
    let value = crate::installed_consumers::account::check().map_err(|_| unavailable())?;
    let proof: ConsumerCapability = serde_json::from_value(value).map_err(|_| unavailable())?;
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

    #[cfg(target_os = "linux")]
    fn admitted(value: serde_json::Value) -> bool {
        serde_json::from_value::<ConsumerCapability>(value)
            .is_ok_and(|proof| proof.supports_service_session_protocol(2))
    }

    // The installed producer's real measured closure over fixture files.
    #[cfg(target_os = "linux")]
    #[test]
    fn measured_closure_is_admitted_and_mutations_fail_closed() {
        let bytes = crate::installed_consumers::tests::account::route_contract_receipt();
        let proof: ConsumerCapability = serde_json::from_slice(&bytes).unwrap();
        proof.validate().unwrap();
        assert!(proof.supports_service_session_protocol(2));
        let valid: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        for field in [
            "sdk_runtime_root_hash",
            "browser_package_hash",
            "browser_publisher",
            "browser_binary_sha256",
            "cef_runtime_version",
            "cef_runtime_root_hash",
            "cef_runtime_publisher",
            "cef_host_sha256",
            "qualification_script_sha256",
            "receiving_shell_sha256",
            "receiving_entry_sha256",
            "receiving_python_sha256",
            "receiving_route_qualification_sha256",
            "origin_storage_qualification_sha256",
        ] {
            let mut changed = valid.clone();
            changed["service_sessions"][field] = "".into();
            assert!(!admitted(changed), "{field}");
        }
        for (field, value) in [
            ("schema", serde_json::json!(1)),
            ("protocol", serde_json::json!(3)),
            ("receiving_shell_path", serde_json::json!("/tmp/shell")),
            (
                "receiving_python_path",
                serde_json::json!("/usr/bin/../bin/python3"),
            ),
            ("sdk_runtime_root_hash", serde_json::json!("e".repeat(64))),
            ("receiving_entry_sha256", serde_json::json!("e".repeat(64))),
            ("receiving_python_sha256", serde_json::json!("e".repeat(64))),
            (
                "origin_storage_qualification_sha256",
                serde_json::json!("e".repeat(64)),
            ),
            (
                "receiving_route_qualification_sha256",
                serde_json::json!(
                    proof
                        .service_sessions
                        .as_ref()
                        .unwrap()
                        .receiving_shell_sha256
                ),
            ),
        ] {
            let mut changed = valid.clone();
            changed["service_sessions"][field] = value;
            assert!(!admitted(changed), "{field}");
        }
        for path in RECEIVING_DEPENDENCIES {
            for remove in [false, true] {
                let mut changed = valid.clone();
                let dependencies = changed["service_sessions"]["receiving_dependencies"]
                    .as_object_mut()
                    .unwrap();
                if remove {
                    dependencies.remove(*path);
                } else {
                    dependencies.insert((*path).into(), serde_json::json!("e".repeat(64)));
                }
                assert!(!admitted(changed), "{path}");
            }
        }
        for field in [
            "receiving_dependencies",
            "receiving_python_path",
            "receiving_entry_sha256",
        ] {
            let mut changed = valid.clone();
            changed["service_sessions"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(!admitted(changed), "{field}");
        }
        let mut changed = valid.clone();
        changed["service_sessions"]["receiving_dependencies"]["/tmp/extra.py"] =
            serde_json::json!("e".repeat(64));
        assert!(!admitted(changed));
        let mut changed = valid.clone();
        changed["service_sessions"]["ready"] = true.into();
        assert!(!admitted(changed));

        let text = String::from_utf8(bytes).unwrap();
        let key = format!("\"{}\":", RECEIVING_DEPENDENCIES[0]);
        let duplicate = text.replacen(&key, &format!("{key}\"{}\",{key}", "e".repeat(64)), 1);
        assert!(serde_json::from_str::<ConsumerCapability>(&duplicate).is_err());

        assert_recomputed_closure_refused(&proof);
        assert_protocol_compatibility(&proof, valid);
    }

    #[cfg(target_os = "linux")]
    fn assert_protocol_compatibility(proof: &ConsumerCapability, valid: serde_json::Value) {
        let mut cookies = proof.clone();
        let service = cookies.service_sessions.as_mut().unwrap();
        service.protocol = 1;
        service.origin_storage_qualification_sha256 = None;
        assert!(cookies.supports_service_sessions());
        assert!(!cookies.supports_service_session_protocol(2));

        let mut legacy = valid;
        legacy.as_object_mut().unwrap().remove("service_sessions");
        let legacy: ConsumerCapability = serde_json::from_value(legacy).unwrap();
        legacy.validate().unwrap();
        assert!(!legacy.supports_service_sessions());
        assert!(!proof.supports_service_session_protocol(3));
    }

    #[cfg(target_os = "linux")]
    fn assert_recomputed_closure_refused(proof: &ConsumerCapability) {
        // Recomputing the digest cannot admit an expanded/replaced dependency
        // set, nor break the entrypoint's binding to its inventory member.
        let mut changed = proof.clone();
        let service = changed.service_sessions.as_mut().unwrap();
        service
            .receiving_dependencies
            .insert("/tmp/extra.py".into(), "e".repeat(64));
        service.receiving_route_qualification_sha256 = service.measured_closure_hash().unwrap();
        assert!(changed.validate().is_err());
        let mut changed = proof.clone();
        let service = changed.service_sessions.as_mut().unwrap();
        service.receiving_entry_sha256 = "e".repeat(64);
        service.receiving_route_qualification_sha256 = service.measured_closure_hash().unwrap();
        assert!(changed.validate().is_err());
    }
}
