//! A person's devices as a test runs them: each an identified actor on a
//! loopback endpoint with its own workspace host open, as every desktop's is,
//! keeping a session with each of the person's devices it is told of
//! ([`crate::NetCmd::Connect`]).
use std::path::Path;
use std::sync::{Arc, Mutex};

use iroh::EndpointAddr;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use tokio::sync::mpsc;

use crate::own_service::workspace_service::{ServiceConfig, WorkspaceService};
use crate::own_service::{OwnService, workspace_service};
use crate::{Actor, DeviceIdentity, NetCmd, OwnServing};

/// One of the person's devices, running.
pub struct Sibling {
    pub cmd: mpsc::Sender<NetCmd>,
    pub task: tokio::task::JoinHandle<()>,
    /// Where the device is reached: what another of the person's devices
    /// is told with [`NetCmd::Connect`].
    pub addr: EndpointAddr,
    /// The device's endpoint.
    pub endpoint: iroh::Endpoint,
    pub cert: DeviceCert,
    /// Where the device looks up addresses of devices it is told of.
    pub lookup: iroh::address_lookup::memory::MemoryLookup,
}

/// The device `key` of the person `root`, keeping its host's state under
/// `state`, with its host open and its actor told so.
///
/// # Panics
///
/// The certificate cannot be signed or the host does not open: a test has
/// nothing to run on then.
pub async fn spawn(
    root: &ed25519_dalek::SigningKey,
    key: ed25519_dalek::SigningKey,
    state: &Path,
) -> Sibling {
    spawn_ticking(root, key, state, crate::HEARTBEAT_INTERVAL).await
}

/// [`spawn`], retrying what is missing every `heartbeat`.
///
/// # Panics
///
/// As [`spawn`].
#[expect(
    clippy::expect_used,
    reason = "a test whose device does not start has nothing to run on"
)]
pub(crate) async fn spawn_ticking(
    root: &ed25519_dalek::SigningKey,
    key: ed25519_dalek::SigningKey,
    state: &Path,
    heartbeat: std::time::Duration,
) -> Sibling {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "sibling".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).expect("sign the device's certificate");
    let (endpoint, lookup) =
        crate::loopback_endpoint(Some(iroh::SecretKey::from_bytes(&key.to_bytes()))).await;
    let addr = endpoint.addr();
    let (local, protocols) = OwnService::deferred();
    let identity = DeviceIdentity::kept(cert.clone(), key.clone());
    let (cmd, task) = Actor::spawn_with_heartbeat(
        endpoint.clone(),
        lookup.clone(),
        Some(identity),
        Arc::new(Mutex::new(Vec::new())),
        heartbeat,
        tilo_endpoint_reachability::Reachability::default(),
        Some(OwnServing {
            service: local.clone(),
            protocols,
        }),
        Vec::new(),
    );
    let store = state.join("workspace-service");
    let service_identity = workspace_service::ServiceIdentity::beside_device(&store, &cert, &key)
        .await
        .expect("the device certifies its workspace service");
    let mut config = ServiceConfig::new(store, tilo_server_protocol::WorkSetting::none());
    // A loopback endpoint is never reached from another network: waiting to
    // learn such an address would hold each device back for the whole wait
    // before its workspace service serves.
    config.reaching = tilo_endpoint_reachability::Reaching::OnItsOwn;
    let host = WorkspaceService::open(config, service_identity, endpoint.clone(), None)
        .await
        .expect("the device's host opens");
    local.install(Arc::new(host));
    cmd.send(NetCmd::OwnServiceSettled)
        .await
        .expect("the actor is running");
    Sibling {
        cmd,
        task,
        addr,
        endpoint,
        cert,
        lookup,
    }
}
