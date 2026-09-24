//! Explicit nonsecret managed-runtime inputs; no ambient package or identity discovery.
use launcher::managed::{InstalledManagedPackage, ManagedLauncher, ManagedRuntime, PackageTrust};
use launcher::services::ManagedProviderSelection;
use serde::Deserialize;
use std::{
    io::Read,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManagedConfig {
    version: u16,
    package_hash: String,
    package_payload: PathBuf,
    package_manifest: PathBuf,
    package_data_root: PathBuf,
    trusted_publishers: Vec<String>,
    trusted_attestors: PathBuf,
    revoked_keys: PathBuf,
    runtime_bundle: PathBuf,
    runtime_publisher: String,
    sandbox_shim: PathBuf,
    bwrap: PathBuf,
    pub(crate) agent_data_home: PathBuf,
    gateway_socket: PathBuf,
    pub(crate) provider: ManagedProviderSelection,
}
impl ManagedConfig {
    pub(crate) fn load(root: &Path) -> Result<Option<Self>, String> {
        let path = root.join("agent-runtime.json");
        let bytes = match bounded_read(&path, 64 * 1024) {
            Ok(bytes) => bytes,
            Err(_error) if !path.exists() => return Ok(None),
            Err(error) => return Err(error),
        };
        let value: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("invalid managed runtime configuration: {e}"))?;
        if value.version != 1
            || value.trusted_publishers.is_empty()
            || value.trusted_publishers.len() > 64
        {
            return Err("unsupported managed runtime configuration".into());
        }
        for key in value
            .trusted_publishers
            .iter()
            .chain(std::iter::once(&value.package_hash))
            .chain(std::iter::once(&value.runtime_publisher))
        {
            decode_hash(key)?;
        }
        for path in [
            &value.package_payload,
            &value.package_manifest,
            &value.package_data_root,
            &value.trusted_attestors,
            &value.revoked_keys,
            &value.runtime_bundle,
            &value.sandbox_shim,
            &value.bwrap,
            &value.agent_data_home,
            &value.gateway_socket,
        ] {
            if !path.is_absolute() {
                return Err("managed runtime paths must be explicit absolute paths".into());
            }
        }
        // This is the existing vault namespace, narrower than the launcher descriptor bound.
        if value.provider.profile_id.is_empty()
            || value.provider.profile_id.len() > 128
            || !value
                .provider
                .profile_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.provider.model_id.is_empty()
            || value.provider.model_id.len() > 256
            || value.provider.model_id.chars().any(char::is_control)
            || !matches!(
                value.provider.protocol.as_str(),
                "anthropic" | "open_ai_compatible"
            )
        {
            return Err("invalid managed Plan selection".into());
        }
        Ok(Some(value))
    }
    pub(crate) fn launcher(
        &self,
        owner: Arc<crate::agent_runtime_adapter::RuntimeOwner>,
    ) -> Result<ManagedLauncher, String> {
        let trust = PackageTrust {
            publishers: self
                .trusted_publishers
                .iter()
                .map(|key| decode_hash(key))
                .collect::<Result<_, _>>()?,
            attestors: tilo_app_package::trust::TrustedAttestors::from_toml_str(
                &String::from_utf8(bounded_read(&self.trusted_attestors, 1024 * 1024)?)
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .attestor,
            revoked: tilo_app_package::trust::RevocationList::from_toml_str(
                &String::from_utf8(bounded_read(&self.revoked_keys, 1024 * 1024)?)
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?,
        };
        let package = InstalledManagedPackage::verify(
            launcher::registry::InstalledPackage {
                id: "so.tilo.agent".into(),
                package_id_hex: self.package_hash.clone(),
                payload_root: self.package_payload.clone(),
                manifest_path: self.package_manifest.clone(),
            },
            self.package_data_root.clone(),
            tilo_app_package::PackageId::from_bytes(decode_hash(&self.package_hash)?),
            &trust,
        )?;
        ManagedLauncher::new(
            package,
            trust,
            ManagedRuntime {
                bundle_root: self.runtime_bundle.clone(),
                runtime_publisher: self.runtime_publisher.clone(),
                sandbox_shim: self.sandbox_shim.clone(),
                bwrap: self.bwrap.clone(),
            },
            owner,
        )
    }
    /// Only proves current local Plan configuration, never model execution or reachability.
    pub(crate) async fn configured_for_plan(&self) -> Result<(), String> {
        self.configured_for_plan_until(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
    }
    async fn configured_for_plan_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<(), String> {
        if tokio::time::Instant::now() >= deadline {
            return Err("gateway configuration deadline exceeded".into());
        }
        tokio::time::timeout_at(deadline, async {
            let mut stream = tokio::net::UnixStream::connect(&self.gateway_socket)
                .await
                .map_err(|e| format!("configured model descriptor unavailable: {e}"))?;
            write_gateway_frame(
                &mut stream,
                &serde_json::json!({"protocol":1,"app_id":"so.tilo.agent"}),
            )
            .await?;
            // Decode through the actual gateway DTO, including its canonical protocol enum.
            let request: tilo_model_gateway::Request = serde_json::from_value(serde_json::json!({
                "verb":"selection_configured", "profile_id":self.provider.profile_id,
                "model_id":self.provider.model_id, "protocol":self.provider.protocol
            }))
            .map_err(|e| e.to_string())?;
            write_gateway_frame(
                &mut stream,
                &tilo_model_gateway::Frame {
                    version: tilo_model_gateway::RPC_VERSION,
                    value: request,
                },
            )
            .await?;
            let response = read_gateway_response(&mut stream).await?;
            if !response.ok
                || response.configured != Some(true)
                || response.output.is_some()
                || response.error.is_some()
            {
                return Err("selected gateway Plan configuration unavailable".into());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("gateway configuration deadline exceeded".into());
            }
            Ok(())
        })
        .await
        .map_err(|_| "gateway configuration deadline exceeded".to_string())?
    }
    pub(crate) async fn model_connection(&self) -> Result<UnixStream, String> {
        self.model_connection_until(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
    }
    async fn model_connection_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<UnixStream, String> {
        if tokio::time::Instant::now() >= deadline {
            return Err("gateway connection deadline exceeded".into());
        }
        let stream = tokio::time::timeout_at(
            deadline,
            tokio::net::UnixStream::connect(&self.gateway_socket),
        )
        .await
        .map_err(|_| "gateway connection deadline exceeded".to_string())?
        .map_err(|e| format!("configured model descriptor unavailable: {e}"))?;
        if tokio::time::Instant::now() >= deadline {
            return Err("gateway connection deadline exceeded".into());
        }
        let stream = stream.into_std().map_err(|e| e.to_string())?;
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        Ok(stream)
    }
}
// Existing gateway wire framing, asynchronously driven under one enclosing deadline.
// Cancellation drops the owned stream; no blocking connector or detached worker survives stop.
async fn write_gateway_frame<T: serde::Serialize>(
    stream: &mut tokio::net::UnixStream,
    value: &T,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let body = zeroize::Zeroizing::new(serde_json::to_vec(value).map_err(|e| e.to_string())?);
    if body.len() > tilo_model_gateway::MAX_FRAME_BYTES {
        return Err("gateway request exceeds frame limit".into());
    }
    stream
        .write_all(&(body.len() as u32).to_le_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(&body).await.map_err(|e| e.to_string())?;
    stream.flush().await.map_err(|e| e.to_string())
}
async fn read_gateway_response(
    stream: &mut tokio::net::UnixStream,
) -> Result<tilo_model_gateway::Response, String> {
    use tokio::io::AsyncReadExt;
    let mut length = [0; 4];
    stream
        .read_exact(&mut length)
        .await
        .map_err(|e| e.to_string())?;
    let length = u32::from_le_bytes(length) as usize;
    if length > tilo_model_gateway::MAX_FRAME_BYTES {
        return Err("gateway response exceeds frame limit".into());
    }
    let mut body = zeroize::Zeroizing::new(vec![0; length]);
    stream
        .read_exact(&mut body)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("invalid gateway response: {e}"))
}
fn decode_hash(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("managed trust/hash must be canonical lower hex".into());
    }
    hex::decode(value)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "invalid managed hash".into())
}
fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = vec![];
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("managed configuration exceeds its byte limit".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, os::fd::AsRawFd, os::unix::net::UnixListener};
    use tokio::time::Instant;

    fn configuration(socket: &Path) -> ManagedConfig {
        serde_json::from_value(serde_json::json!({
            "version":1,"package_hash":"00".repeat(32),"package_payload":"/unused",
            "package_manifest":"/unused","package_data_root":"/unused",
            "trusted_publishers":["00".repeat(32)],"trusted_attestors":"/unused",
            "revoked_keys":"/unused","runtime_bundle":"/unused",
            "runtime_publisher":"00".repeat(32),"sandbox_shim":"/unused",
            "bwrap":"/unused","agent_data_home":"/unused","gateway_socket":socket,
            "provider":{"profile_id":"configured-profile","model_id":"model","protocol":"anthropic"}
        }))
        .unwrap()
    }
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }
    #[test]
    fn gateway_probe_whole_frame_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let handoff: serde_json::Value = tilo_model_gateway::read_frame(&mut socket).unwrap();
            assert_eq!(handoff["app_id"], "so.tilo.agent");
            let request: tilo_model_gateway::Frame<tilo_model_gateway::Request> =
                tilo_model_gateway::read_frame(&mut socket).unwrap();
            assert_eq!(request.version, tilo_model_gateway::RPC_VERSION);
            assert!(matches!(
                request.value,
                tilo_model_gateway::Request::SelectionConfigured { .. }
            ));
            let body = br#"{"ok":true,"configured":true}"#;
            let frame = [&(body.len() as u32).to_le_bytes()[..], body].concat();
            for byte in frame {
                if socket.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        });
        let start = std::time::Instant::now();
        let result = runtime().block_on(
            configuration(&path)
                .configured_for_plan_until(Instant::now() + Duration::from_millis(90)),
        );
        let elapsed = start.elapsed();
        server.join().unwrap();
        assert!(
            result.is_err(),
            "slow successful bytes must not renew the deadline"
        );
        assert!(
            elapsed < Duration::from_millis(350),
            "whole-frame deadline exceeded: {elapsed:?}"
        );
    }
    #[test]
    fn gateway_full_backlog_deadline_keeps_executor_live() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.sock");
        let listener = UnixListener::bind(&path).unwrap();
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
        let fillers = runtime().block_on(async {
            let mut held = vec![];
            for _ in 0..64 {
                match tokio::time::timeout(
                    Duration::from_millis(30),
                    tokio::net::UnixStream::connect(&path),
                )
                .await
                {
                    Ok(Ok(stream)) => held.push(stream),
                    _ => return held,
                }
            }
            panic!("listener backlog was not saturated")
        });
        assert!(!fillers.is_empty());
        let config = configuration(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            tokio::task::LocalSet::new().block_on(&runtime(), async {
                let query = tokio::task::spawn_local(async move {
                    config
                        .model_connection_until(Instant::now() + Duration::from_millis(100))
                        .await
                });
                tokio::time::sleep(Duration::from_millis(25)).await;
                tx.send(()).unwrap();
                let result = query.await.unwrap();
                assert!(
                    result.is_err(),
                    "full backlog cannot authorize a descriptor"
                );
            })
        });
        let alive = rx.recv_timeout(Duration::from_millis(250)).is_ok();
        drop(listener);
        drop(fillers);
        let joined = worker.join();
        assert!(alive, "gateway connect blocked its owner executor");
        joined.unwrap();
    }
    #[test]
    fn gateway_probe_stalled_response_cancellation_closes_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (received, pending) = tokio::sync::oneshot::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let _: serde_json::Value = tilo_model_gateway::read_frame(&mut stream).unwrap();
            let _: tilo_model_gateway::Frame<tilo_model_gateway::Request> =
                tilo_model_gateway::read_frame(&mut stream).unwrap();
            stream.write_all(&100u32.to_le_bytes()).unwrap();
            stream.write_all(b"{").unwrap(); // Incomplete body, with no blocking worker to join.
            received.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(
                stream.read(&mut byte).unwrap(),
                0,
                "cancel must close the actual gateway socket"
            );
        });
        runtime().block_on(async {
            let config = configuration(&path);
            let task = tokio::spawn(async move { config.configured_for_plan().await });
            tokio::time::timeout(Duration::from_secs(1), pending)
                .await
                .unwrap()
                .unwrap();
            task.abort();
            let result = tokio::time::timeout(Duration::from_millis(200), task)
                .await
                .unwrap();
            assert!(result.unwrap_err().is_cancelled());
        });
        server.join().unwrap();
    }
    #[test]
    fn gateway_probe_accepts_only_exact_configured_response_and_frame_bound() {
        for (response, valid) in [
            (serde_json::json!({"ok":true,"configured":true}), true),
            (serde_json::json!({"ok":true,"configured":false}), false),
            (serde_json::json!({"ok":true}), false),
            (serde_json::json!({"ok":false,"configured":true}), false),
            (
                serde_json::json!({"ok":true,"configured":true,"output":"unexpected"}),
                false,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("gateway.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let _: serde_json::Value = tilo_model_gateway::read_frame(&mut stream).unwrap();
                let request: tilo_model_gateway::Frame<tilo_model_gateway::Request> =
                    tilo_model_gateway::read_frame(&mut stream).unwrap();
                match request.value {
                    tilo_model_gateway::Request::SelectionConfigured {
                        profile_id,
                        model_id,
                        ..
                    } => {
                        assert_eq!(profile_id, "configured-profile");
                        assert_eq!(model_id, "model");
                    }
                    _ => panic!("query must never call Plan"),
                }
                tilo_model_gateway::write_frame(&mut stream, &response).unwrap();
            });
            let result = runtime().block_on(configuration(&path).configured_for_plan());
            server.join().unwrap();
            assert_eq!(result.is_ok(), valid);
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _: serde_json::Value = tilo_model_gateway::read_frame(&mut stream).unwrap();
            let _: serde_json::Value = tilo_model_gateway::read_frame(&mut stream).unwrap();
            stream
                .write_all(&((tilo_model_gateway::MAX_FRAME_BYTES + 1) as u32).to_le_bytes())
                .unwrap();
        });
        let result = runtime().block_on(configuration(&path).configured_for_plan());
        server.join().unwrap();
        assert!(result.unwrap_err().contains("exceeds frame limit"));
    }
    #[test]
    fn gateway_connected_model_handoff_is_original_blocking_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let mut model = runtime()
            .block_on(configuration(&path).model_connection())
            .unwrap();
        let flags = unsafe { libc::fcntl(model.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(flags & libc::O_NONBLOCK, 0);
        model.write_all(b"exact-connected-stream").unwrap();
        let (mut accepted, _) = listener.accept().unwrap();
        let mut bytes = [0; 22];
        accepted.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"exact-connected-stream");
    }
}
