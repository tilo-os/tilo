//! The devices, endpoints and workspaces the two-device tests run on.
#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these fixtures"
)]

use ed25519_dalek::SigningKey;

use tilo_peer_state::StatePeer;
use tilo_peer_state::direct::DirectSync;
pub use tilo_peer_state::sealed_records::{
    members_keys, opened, opened_heads, sealed_write, write_sealed,
};
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

pub use tilo_peer_state::loopback::{addr, endpoint as localhost_endpoint};

/// The workspace `workspace`, made by `a` with `roster` in it, held as an
/// empty peer.
pub fn workspace(
    workspace: &str,
    a: &(SigningKey, DeviceCert),
    roster: &[DeviceCert],
) -> StatePeer {
    let now = tilo_clock::wall();
    let mut peer = StatePeer::new(
        workspace,
        a.1.clone(),
        roster.to_vec(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.select_exact_admissions().unwrap();
    tilo_peer_state::fixture::found(&mut peer, (&a.0, &a.1), roster, Vec::new(), now);
    peer
}

/// `sync`, keeping what it holds and is sent in a store in `directory`, as
/// every device holding a workspace does.
pub fn stored(sync: &DirectSync, directory: &tempfile::TempDir) -> DirectSync {
    let store = tilo_peer_state::store::Store::open(
        directory.path(),
        tilo_peer_state::store::Appetite::StoreAll,
    )
    .unwrap();
    sync.with_store(std::sync::Arc::new(store))
}
