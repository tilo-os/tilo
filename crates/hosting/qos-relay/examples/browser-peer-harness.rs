//! Loopback-only real Iroh packet relay + certified qOS relay/1 fixture.
//! Credentials are test-only, returned in response bodies, never URLs or logs.
use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use iroh::{Endpoint, RelayMap, RelayMode, endpoint::presets};
use iroh_base::{EndpointAddr, RelayUrl};
use iroh_relay::server::{RelayConfig as PacketConfig, Server, ServerConfig};
use qos_relay::{
    Envelope, Relay, RelayClient, RelayConfig, WorkspaceBinding, daemon::RelayIdentity,
};
use qos_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity,
    WorkspaceDirectory, sign_device_cert,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::Ipv4Addr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

struct Enrolled {
    key: SigningKey,
    cert: DeviceCert,
}
fn enrol(root: &SigningKey, name: &str, capabilities: DeviceCapabilities) -> Result<Enrolled> {
    let now = qos_relay::clock::now_ns();
    let (key, device) = DeviceIdentity::generate(name, now);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.public_key,
        device_label: name.into(),
        capabilities,
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(24 * 60 * 60 * 1_000_000_000)),
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root)?;
    Ok(Enrolled { key, cert })
}
async fn endpoint(mode: RelayMode) -> Result<Endpoint> {
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(mode)
        .bind_addr((Ipv4Addr::LOCALHOST, 0))?
        .bind()
        .await?;
    tokio::time::timeout(Duration::from_secs(15), endpoint.online())
        .await
        .context("packet relay did not become online")?;
    Ok(endpoint)
}
async fn serve(
    mut socket: TcpStream,
    routes: Arc<HashMap<String, Value>>,
    received: Arc<AtomicU64>,
    connected: Arc<AtomicBool>,
) -> Result<()> {
    // This serves enrollment fixtures only. Application state travels over the
    // authenticated Iroh QUIC stream; there is no HTTP/JSON sync gateway.
    let mut request = Vec::new();
    let mut chunk = [0u8; 2048];
    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
        let count = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut chunk)).await??;
        if count == 0 {
            return Ok(());
        }
        request.extend_from_slice(&chunk[..count]);
        if request.len() > 8192 {
            bail!("fixture request too large");
        }
    }
    let request = std::str::from_utf8(&request)?;
    let mut lines = request.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut words = first.split_whitespace();
    let method = words.next().unwrap_or_default();
    let path = words.next().unwrap_or_default();
    let headers: HashMap<_, _> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim()))
        .collect();
    let allowed_origin = "http://127.0.0.1:9998";
    let forbidden = headers
        .get("origin")
        .is_some_and(|origin| *origin != allowed_origin)
        || !headers
            .get("host")
            .is_some_and(|host| host.starts_with("127.0.0.1:"));
    let (code, body) = if forbidden {
        (403, json!({"error":"fixture origin is not permitted"}))
    } else if method != "GET" {
        (405, json!({"error":"GET required"}))
    } else if path == "/status" {
        (
            200,
            json!({"native_peer":if connected.load(Ordering::Relaxed) { "subscribed" } else { "disconnected" }, "received_envelopes":received.load(Ordering::Relaxed), "application_state":"phase B integration pending"}),
        )
    } else if let Some(value) = routes.get(path) {
        (200, value.clone())
    } else {
        (404, json!({"error":"fixture not found"}))
    };
    let body = serde_json::to_vec(&body)?;
    let response = format!(
        "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: {allowed_origin}\r\nVary: Origin\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.write_all(&body).await?;
    socket.shutdown().await?;
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--ready-file")) {
        bail!("usage: browser-peer-harness --ready-file /absolute/ready.json");
    }
    let ready_path = PathBuf::from(args.next().context("ready-file path required")?);
    if !ready_path.is_absolute() || args.next().is_some() {
        bail!("one absolute ready-file path is required");
    }
    let state = tempfile::Builder::new()
        .prefix("qos-browser-peer-")
        .tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    let (root, user) = UserIdentity::generate(
        "Isolated browser interoperability workspace",
        qos_relay::clock::now_ns(),
    );
    let host = enrol(&root, "Local native relay", DeviceCapabilities::host())?;
    let native = enrol(
        &root,
        "Native workspace peer",
        DeviceCapabilities::primary(),
    )?;
    let a = enrol(&root, "Browser A", DeviceCapabilities::member())?;
    let b = enrol(&root, "Browser B", DeviceCapabilities::member())?;
    let room = uuid::Uuid::new_v4().to_string();
    let workspace_id = qos_relay::room_workspace_id(&room)?;
    let mut packet_config = ServerConfig::default();
    packet_config.relay = Some(PacketConfig::new((Ipv4Addr::LOCALHOST, 0)));
    let packet = Server::spawn(packet_config).await?;
    let packet_url: RelayUrl = format!(
        "http://{}",
        packet
            .http_addr()
            .context("packet relay did not bind HTTP")?
    )
    .parse()?;
    let mode = RelayMode::Custom(RelayMap::from(packet_url.clone()));
    let config = RelayConfig {
        state_root: state.path().to_owned(),
        identity_root: state.path().join("identity"),
        full_replica_status: state.path().join("full-replica/status.json"),
        home_peer_bundle: state.path().join("home-peer.toml"),
        trust_policy: qos_relay::TrustPolicy::AllowList {
            users: vec![hex::encode(user.public_key)],
        },
        iroh_listen: (Ipv4Addr::LOCALHOST, 0).into(),
        workspaces: vec![WorkspaceBinding {
            id: workspace_id,
            pin: true,
            replication: qos_relay::ReplicationIntent::FullWorkspace,
            admitted_users: vec![],
        }],
        ..Default::default()
    };
    let relay = Relay::start_with_relay_mode(
        config,
        RelayIdentity {
            user_public_key: user.public_key,
            device_signing_key: host.key.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
        mode.clone(),
    )
    .await?;
    relay
        .set_workspace_membership(
            workspace_id,
            true,
            vec![
                native.cert.device_public_key,
                a.cert.device_public_key,
                b.cert.device_public_key,
            ],
            state.path().to_owned(),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(15), relay.endpoint.online())
        .await
        .context("qOS endpoint did not become online")?;
    let addr = EndpointAddr::new(relay.node_addr().id).with_relay_url(packet_url.clone());
    let native_client = RelayClient::connect_to_enrolled_home_peer(
        endpoint(mode.clone()).await?,
        addr.clone(),
        user.public_key,
        host.cert.device_public_key,
        native.cert.clone(),
        native.key.clone(),
        RevocationList::new(),
    )
    .await?;
    native_client
        .send(Envelope::Subscribe { workspace_id })
        .map_err(|_| anyhow::anyhow!("native subscribe failed"))?;
    let ready = tokio::time::timeout(Duration::from_secs(15), native_client.recv()).await?;
    if !matches!(ready, Some(Envelope::Ready { workspace_id: id, .. }) if id == workspace_id) {
        bail!("native peer did not receive workspace Ready");
    }
    let mut directory = WorkspaceDirectory::new(user.public_key, native.cert.device_public_key);
    directory.add_workspace(workspace_id, "Shared browser workspace")?;
    directory.set_member_devices(
        workspace_id,
        &[
            native.cert.device_public_key,
            a.cert.device_public_key,
            b.cert.device_public_key,
        ],
    )?;
    directory.set_host_devices(workspace_id, &[host.cert.device_public_key])?;
    let signed_directory = directory.sync_incremental(&native.key, qos_relay::clock::now_ns())?;
    // Stable, signed bootstrap metadata within this process. Phase B must still
    // admit shell governance and signed app records before applying app state.
    let bootstrap = json!({"directory_update":signed_directory, "directory_author":native.cert, "certified_roster":[native.cert,a.cert,b.cert], "room":room, "workspace_id":workspace_id.to_string(), "writer_tab_id":1, "chat_package_primary":true});
    let mut routes = HashMap::new();
    for (name, browser) in [("browser-a", a), ("browser-b", b)] {
        routes.insert(format!("/enrollment/{name}"), json!({
            "version":1, "device_secret":browser.key.to_bytes(), "certificate":browser.cert,
            "root_public_key":user.public_key, "host_certificate":host.cert,
            "relay_address":addr, "packet_relay":packet_url, "revocations":Vec::<qos_user_identity::RevocationEntry>::new(),
            "bootstrap":bootstrap,
        }));
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let fixture_addr = listener.local_addr()?;
    let received = Arc::new(AtomicU64::new(0));
    let count = received.clone();
    let connected = Arc::new(AtomicBool::new(true));
    let native_connected = connected.clone();
    let native_task = tokio::spawn(async move {
        while let Some(envelope) = native_client.recv().await {
            if matches!(envelope, Envelope::NotAuthorised { .. }) {
                break;
            }
            count.fetch_add(1, Ordering::Relaxed);
        }
        native_connected.store(false, Ordering::Relaxed);
    });
    let metadata = json!({"version":1,"status":"native-peer-subscribed", "pid":std::process::id(), "platform":std::env::consts::OS,
        "fixture_origin":format!("http://{fixture_addr}"), "enrollments":["/enrollment/browser-a","/enrollment/browser-b"],
        "status_path":"/status", "packet_relay":packet_url, "relay_address":addr, "room":room,
        "workspace_id":workspace_id.to_string(), "chat_package_primary":true,
        "application_state":"phase B integration pending", "state_root":state.path() });
    if let Some(parent) = ready_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&ready_path, serde_json::to_vec_pretty(&metadata)?)?;
    println!(
        "Native certified peer and loopback packet relay ready; metadata: {}",
        ready_path.display()
    );
    let routes = Arc::new(routes);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            result = listener.accept() => {
                let (socket, _) = result?;
                let routes = routes.clone(); let received = received.clone(); let connected = connected.clone();
                tokio::spawn(async move { let _ = serve(socket, routes, received, connected).await; });
            }
        }
    }
    native_task.abort();
    relay.shutdown().await;
    packet.shutdown().await?;
    let _ = std::fs::remove_file(ready_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn response(path: &str, origin: Option<&str>, connected: bool) -> String {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut routes = HashMap::new();
        routes.insert(
            "/enrollment/browser-a".to_owned(),
            json!({"fixture_secret":"do-not-disclose"}),
        );
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            serve(
                socket,
                Arc::new(routes),
                Arc::new(AtomicU64::new(3)),
                Arc::new(AtomicBool::new(connected)),
            )
            .await
            .unwrap();
        });
        let mut socket = TcpStream::connect(addr).await.unwrap();
        let origin_header = origin
            .map(|value| format!("Origin: {value}\r\n"))
            .unwrap_or_default();
        socket
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n{origin_header}\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).await.unwrap();
        task.await.unwrap();
        response
    }

    #[tokio::test]
    async fn unrelated_web_origin_cannot_read_fixture_credentials() {
        let result = response(
            "/enrollment/browser-a",
            Some("https://unrelated.invalid"),
            true,
        )
        .await;
        assert!(result.starts_with("HTTP/1.1 403"));
        assert!(!result.contains("do-not-disclose"));
    }

    #[tokio::test]
    async fn public_status_reports_actual_native_disconnect_without_credentials() {
        let result = response("/status", Some("http://127.0.0.1:9998"), false).await;
        assert!(result.starts_with("HTTP/1.1 200"));
        assert!(result.contains("\"native_peer\":\"disconnected\""));
        assert!(!result.contains("do-not-disclose"));
        assert!(result.contains("phase B integration pending"));
    }
}
