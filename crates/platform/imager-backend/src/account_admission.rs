//! Admission of protected account custody by the selected image and actual
//! installed consumers. A staged importer never certifies its own target.
#[cfg(target_os = "linux")]
mod candidate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io};

// This fixed image closure matches RECEIVING_DEPENDENCIES in the producer.
const RECEIVING_DEPENDENCIES: &[&str] = &[
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
        let result = stdout
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = send.send(result);
    });
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(150))
        .ok_or_else(unavailable)?;
    let result = receive.recv_timeout(std::time::Duration::from_secs(150));
    let bytes = match result {
        Ok(Ok(bytes)) if bytes.len() <= 16 * 1024 => bytes,
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

    // This is a serialization/measurement fixture, not native readiness evidence.
    // Execute the real Python route producer with actual files; only the owned
    // sandbox qualification callback is replaced by its explicit test response.
    fn python_contract() -> Vec<u8> {
        let scripts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../os/scripts");
        let script = r"
import importlib.util, json, pathlib, sys, tempfile
scripts = pathlib.Path(sys.argv[1])
def load(name):
    spec = importlib.util.spec_from_file_location(name, scripts / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module
service = load('service-consumer-capability')
verifier = load('verify-account-consumer-capability')
with tempfile.TemporaryDirectory() as temporary:
    root = pathlib.Path(temporary).resolve()
    for path in (*service.RECEIVING_DEPENDENCIES, '/usr/bin/shell', '/usr/bin/python3.13'):
        target = root / path.removeprefix('/')
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(('fixture: ' + path).encode())
        target.chmod(0o755)
    (root / 'usr/bin/python3').symlink_to('python3.13')
    sdk = dict(schema=1, protected_grant_protocol=1,
        credential_helper_sha256='1'*64, runtime_root_hash='2'*64,
        runtime_probe_sha256='3'*64, runtime_library_sha256='4'*64,
        runtime_version='2026.10', runtime_library='lib/libruntime_dylib.so')
    candidate = dict(schema=1, protocol=2, sdk_runtime_root_hash=sdk['runtime_root_hash'],
        browser_package_hash='5'*64, browser_publisher='6'*64,
        browser_binary_sha256='7'*64, cef_runtime_version='2026.10-chromium',
        cef_runtime_root_hash='8'*64, cef_runtime_publisher='9'*64,
        cef_host_sha256='a'*64, qualification_script_sha256='b'*64,
        origin_storage_qualification_sha256='b'*64)
    def fixture_qualifier(command, env, request):
        facts = json.loads(request)
        return json.dumps(dict(schema=2, service_session_protocol=2,
            receiving_route_qualification_sha256=facts['receiving_route_qualification_sha256'],
            private_list_verified=True, private_resolve_verified=True,
            isolated_profile_verified=True, revocation_verified=True,
            no_vault_mount_verified=True, origin_storage_persisted=True,
            chat_destination_navigation_verified=True)).encode()
    primitives = (verifier.digest, verifier.interpreter, verifier.regular)
    # The default callback must never fabricate a successful receiving route.
    try:
        service.qualify_receiving_route(root, sdk, candidate, primitives, '/usr/bin/shell')
    except ValueError:
        pass
    else:
        raise AssertionError('default route qualification fabricated readiness')
    sdk['service_sessions'] = service.qualify_receiving_route(
        root, sdk, candidate, primitives, '/usr/bin/shell', fixture_qualifier)
    receipt = root / 'receipt.json'
    receipt.write_text(json.dumps(sdk, sort_keys=True, separators=(',', ':')))
    # Exercise the installed verifier's strict saved-receipt reader too.
    print(json.dumps(verifier.read_receipt(receipt, sdk), sort_keys=True, separators=(',', ':')))
";
        let output = std::process::Command::new("python3")
            .arg("-I")
            .arg("-c")
            .arg(script)
            .arg(scripts)
            .output()
            .expect("Python contract producer must be available");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.len() <= 16 * 1024);
        output.stdout
    }

    fn admitted(value: serde_json::Value) -> bool {
        serde_json::from_value::<ConsumerCapability>(value)
            .is_ok_and(|proof| proof.supports_service_session_protocol(2))
    }

    #[test]
    fn real_python_measured_closure_is_admitted_and_mutations_fail_closed() {
        let bytes = python_contract();
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
