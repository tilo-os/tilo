//! Pre-staging native admission through the existing owner's semantic endpoint.
//!
//! This connection is not image-update authority. Its caller must hold the
//! updater and VM locks and supply executables from the verified captured view.
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    time::{Duration, Instant},
};

use launcher::run::{NativeAdmissionObservation, NativeAdmissionScope};
use serde_json::{Value, json};
use tilo_control::{ControlClient, KernelPeer};

/// Kernel custody retained for the complete admission sequence. The executable
/// descriptor, rather than a returned path or claimed hash, selects the host.
pub struct AdmissionConnection {
    client: ControlClient,
    peer: KernelPeer,
    native_host_blake3: String,
    launcher_blake3: String,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn digest(file: &mut File) -> io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut bytes = [0; 8192];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            return Ok(hasher.finalize().to_hex().to_string());
        }
        hasher.update(
            bytes
                .get(..count)
                .ok_or_else(|| invalid("invalid host read length"))?,
        );
    }
}

impl AdmissionConnection {
    /// Connect to the ordinary private native profile. Both executable paths
    /// must come from the caller's already verified immutable full cohort.
    ///
    /// # Errors
    /// Refuses foreign UID, a different executed host, missing kernel custody,
    /// missing profile, or an unsupported semantic endpoint.
    pub fn connect(
        socket: &Path,
        owner_uid: u32,
        native_host: &Path,
        launcher: &Path,
    ) -> io::Result<Self> {
        let mut client = ControlClient::connect(socket)?;
        let mut peer = client.observe_peer()?;
        if peer.uid != owner_uid {
            return Err(invalid("checkpoint native host belongs to another account"));
        }
        let host_blake3 = digest(&mut peer.executable)?;
        if host_blake3 != digest(&mut File::open(native_host)?)? {
            return Err(invalid(
                "checkpoint is not served by the captured native host",
            ));
        }
        let launcher_blake3 = digest(&mut File::open(launcher)?)?;
        client.revalidate_peer(&peer)?;
        let status = client.exchange(&json!({"native":"status"}).to_string())?;
        if status.get("version").and_then(Value::as_u64) != Some(1)
            || status.get("pid").and_then(Value::as_u64) != Some(u64::from(peer.pid))
        {
            return Err(invalid("checkpoint native endpoint has no matching status"));
        }
        client.revalidate_peer(&peer)?;
        Ok(Self {
            client,
            peer,
            native_host_blake3: host_blake3,
            launcher_blake3,
        })
    }

    fn call(&mut self, name: &str, arguments: &Value, deadline: Instant) -> io::Result<Value> {
        self.client.revalidate_peer(&self.peer)?;
        let request = json!({
            "jsonrpc":"2.0", "id":1, "method":"tools/call",
            "params":{"name":name,"arguments":arguments}
        });
        let reply = self
            .client
            .exchange_before(&request.to_string(), deadline)?;
        self.client.revalidate_peer(&self.peer)?;
        if reply.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || reply.get("id") != Some(&json!(1))
            || reply.get("error").is_some()
            || reply.pointer("/result/isError") != Some(&Value::Bool(false))
        {
            return Err(invalid("checkpoint semantic operation was refused"));
        }
        reply
            .pointer("/result/structuredContent")
            .cloned()
            .ok_or_else(|| invalid("checkpoint semantic response has no structured content"))
    }

    /// Observe the exact package through its public native-admission action.
    /// No application payload is opened, and no new action protocol is used.
    /// The caller must supply the full SDK manifest verified from the captured
    /// bytes under the normal publisher or exact retained-runtime authority.
    ///
    /// # Errors
    /// Refuses unsupported targets, nonterminal/refused actions, changed host
    /// custody, foreign signed roots, launcher or workspace, and timeout.
    pub fn observe(
        &mut self,
        scope: NativeAdmissionScope<'_>,
        runtime: &runtime_bundle::BundleManifest,
    ) -> io::Result<NativeAdmissionObservation> {
        // Establish this connection's ordinary catalogue before invoking the
        // resource. The host retains authority over revision and action gates.
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(90))
            .ok_or_else(|| invalid("checkpoint deadline overflow"))?;
        self.call(
            "tilo_query",
            &json!({
                "workspace":true,"needle":scope.package_id,"detail":"full"
            }),
            deadline,
        )?;
        let invocation = tilo_actions::outcome::InvocationId::new();
        let mut response = self.call(
            "tilo_invoke",
            &json!({
                "workspace":true,"resource":format!("app.{}", scope.package_id),
                "action":tilo_actions::remote::APPLICATION_ADMISSION_ACTION,
                "arguments":{},"invocation_id":invocation,"wait_ms":5000
            }),
            deadline,
        )?;
        let value = loop {
            let update: tilo_actions::outcome::InvocationUpdate = serde_json::from_value(
                response
                    .get("update")
                    .cloned()
                    .ok_or_else(|| invalid("checkpoint action has no update"))?,
            )
            .map_err(|error| invalid(format!("invalid checkpoint action update: {error}")))?;
            if update.invocation() != invocation {
                return Err(invalid("checkpoint action belongs to another invocation"));
            }
            match update {
                tilo_actions::outcome::InvocationUpdate::Succeeded { output, .. } => {
                    break output
                        .value
                        .ok_or_else(|| invalid("checkpoint admission returned no evidence"))?;
                }
                tilo_actions::outcome::InvocationUpdate::Accepted { .. }
                | tilo_actions::outcome::InvocationUpdate::Progress { .. }
                | tilo_actions::outcome::InvocationUpdate::Item { .. }
                    if Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(50));
                    response = self.call(
                        "tilo_status",
                        &json!({"invocation_id":invocation}),
                        deadline,
                    )?;
                }
                _ => return Err(invalid("checkpoint native admission did not succeed")),
            }
        };
        let observation: NativeAdmissionObservation = serde_json::from_value(value)
            .map_err(|error| invalid(format!("invalid checkpoint admission evidence: {error}")))?;
        let admission = &observation.admission;
        if observation.native_host_blake3 != self.native_host_blake3
            || admission.launcher_blake3 != self.launcher_blake3
            || admission.package_id != scope.package_id
            || admission.package_root != scope.package_root
            || admission.workspace_id != scope.workspace_id
            || &admission.runtime != runtime
        {
            return Err(invalid(
                "checkpoint admission differs from its captured target or host",
            ));
        }
        self.client.revalidate_peer(&self.peer)?;
        Ok(observation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        thread,
    };

    // This fixture exercises the consumer's protocol and kernel-custody joins.
    // The real launcher/signature/service path has separate native-admission
    // integration tests; a test socket does not claim to run that launcher.
    fn server(
        socket: &Path,
        runtime: runtime_bundle::BundleManifest,
        mismatch: &'static str,
    ) -> io::Result<thread::JoinHandle<io::Result<()>>> {
        let listener = UnixListener::bind(socket)?;
        let executable_hash = digest(&mut File::open("/proc/self/exe")?)?;
        Ok(thread::spawn(move || {
            let (mut stream, _) = listener.accept()?;
            let mut reader = BufReader::new(stream.try_clone()?);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line)? == 0 {
                    return Ok(());
                }
                let request: Value =
                    serde_json::from_str(&line).map_err(|error| invalid(error.to_string()))?;
                let reply = if request == json!({"native":"status"}) {
                    json!({"version":1,"pid":std::process::id(),"room":"workspace"})
                } else {
                    let value = if request.pointer("/params/name") == Some(&json!("tilo_query")) {
                        json!({"rows":[]})
                    } else if mismatch == "pending"
                        && request.pointer("/params/name") == Some(&json!("tilo_invoke"))
                    {
                        let invocation = request
                            .pointer("/params/arguments/invocation_id")
                            .ok_or_else(|| invalid("fixture needs ordinary invocation identity"))?;
                        json!({"update":{"state":"accepted","invocation":invocation}})
                    } else {
                        let mut bundle = runtime.clone();
                        if mismatch == "runtime" {
                            bundle.root_hash = "f0".repeat(32);
                        }
                        let scope = if mismatch == "package" {
                            "another"
                        } else {
                            "fixture"
                        };
                        let invocation = request
                            .pointer("/params/arguments/invocation_id")
                            .ok_or_else(|| invalid("fixture needs ordinary invocation identity"))?;
                        let invocation = if mismatch == "invocation" {
                            json!(uuid::Uuid::new_v4())
                        } else {
                            invocation.clone()
                        };
                        json!({"update":{"state":"succeeded","invocation":invocation,"output":{"value":{
                            "native_host_blake3":executable_hash,
                            "admission":{"package_id":scope,"package_root":"ab".repeat(32),
                                "workspace_id":"workspace","launcher_blake3":executable_hash,
                                "runtime":bundle,"connected_services":[]}
                        }}}})
                    };
                    json!({"jsonrpc":"2.0","id":1,"result":{"isError":false,"structuredContent":value}})
                };
                writeln!(stream, "{reply}")?;
            }
        }))
    }

    fn signed_sdk(root: &Path) -> io::Result<runtime_bundle::BundleManifest> {
        let identity = root.join("identity.json");
        std::fs::write(
            &identity,
            json!({"secret_key_hex":"42".repeat(32)}).to_string(),
        )?;
        let (manifest, bundle) = runtime_bundle::build_abi_bundle(
            &root.join("runtime"),
            "2026.10",
            &runtime_bundle::BuildOptions {
                from: Some(Path::new("/")),
                arch: Some(std::env::consts::ARCH),
                identity: Some(&identity),
                extra_libs: &[],
            },
        )
        .map_err(|error| invalid(error.to_string()))?;
        let verified = runtime_bundle::verify_bundle_with(
            &bundle,
            &runtime_bundle::VerifyOptions {
                require_signature: true,
                require_publisher: manifest.publisher_hex,
                require_arch: Some(std::env::consts::ARCH.into()),
            },
        )
        .map_err(|error| invalid(error.to_string()))?;
        Ok(verified)
    }

    #[test]
    fn semantic_admission_joins_actual_socket_host_and_verified_sdk() {
        let root = tempfile::tempdir().unwrap();
        let sdk = signed_sdk(root.path()).unwrap();
        for mismatch in ["", "pending", "package", "runtime", "invocation"] {
            let socket = root.path().join(format!("native-{mismatch}.sock"));
            let worker = server(&socket, sdk.clone(), mismatch).unwrap();
            // SAFETY: geteuid has no arguments or memory preconditions.
            let uid = unsafe { libc::geteuid() };
            let mut connection = AdmissionConnection::connect(
                &socket,
                uid,
                Path::new("/proc/self/exe"),
                Path::new("/proc/self/exe"),
            )
            .unwrap();
            let result = connection.observe(
                NativeAdmissionScope {
                    package_id: "fixture",
                    package_root: &"ab".repeat(32),
                    workspace_id: "workspace",
                },
                &sdk,
            );
            assert_eq!(
                result.is_ok(),
                matches!(mismatch, "" | "pending"),
                "{mismatch}: {result:?}"
            );
            drop(connection);
            worker.join().unwrap().unwrap();
        }
    }

    #[test]
    fn semantic_admission_refuses_foreign_uid_or_executed_host() {
        let root = tempfile::tempdir().unwrap();
        let sdk = signed_sdk(root.path()).unwrap();
        for foreign_uid in [true, false] {
            let socket = root.path().join(format!("foreign-{foreign_uid}.sock"));
            let worker = server(&socket, sdk.clone(), "").unwrap();
            // SAFETY: geteuid has no arguments or memory preconditions.
            let uid = unsafe { libc::geteuid() };
            let candidate = root.path().join("wrong-host");
            std::fs::write(&candidate, b"unrelated executable").unwrap();
            let result = AdmissionConnection::connect(
                &socket,
                if foreign_uid {
                    uid.checked_add(1).unwrap()
                } else {
                    uid
                },
                if foreign_uid {
                    Path::new("/proc/self/exe")
                } else {
                    &candidate
                },
                Path::new("/proc/self/exe"),
            );
            assert!(result.is_err());
            drop(result);
            worker.join().unwrap().unwrap();
        }
    }
}
