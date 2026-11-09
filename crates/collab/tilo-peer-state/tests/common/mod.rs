//! The devices, endpoints and workspaces the two-device tests run on.
#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these fixtures"
)]
#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use ed25519_dalek::SigningKey;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use web_time::SystemTime;

use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

/// A member device key `n` certified by `root`.
pub fn device(root: &SigningKey, n: u8) -> (SigningKey, DeviceCert) {
    device_with(root, n, DeviceCapabilities::member())
}

/// A device key `n` certified by `root`, with `capabilities`.
pub fn device_with(
    root: &SigningKey,
    n: u8,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{n}"),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// An endpoint whose identity is the device `key`.
pub async fn endpoint(key: &SigningKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .unwrap()
}

/// A localhost endpoint with no relay whose identity is the device `key`.
pub async fn localhost_endpoint(key: &SigningKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

pub fn addr(endpoint: &Endpoint) -> EndpointAddr {
    EndpointAddr::new(endpoint.id()).with_ip_addr(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        endpoint.bound_sockets().first().unwrap().port(),
    )))
}

/// The workspace `workspace`, made by `a` with `roster` in it, held as an
/// empty peer.
pub fn workspace(
    workspace: &str,
    a: &(SigningKey, DeviceCert),
    roster: &[DeviceCert],
) -> StatePeer {
    let now = SystemTime::now();
    let mut peer = StatePeer::new(
        workspace,
        a.1.clone(),
        roster.to_vec(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(workspace, &a.1, roster).unwrap(),
            documents: Vec::new(),
        },
        &a.0,
        now,
    )
    .unwrap();
    peer
}
