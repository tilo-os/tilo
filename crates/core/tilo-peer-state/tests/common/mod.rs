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
use tilo_user_identity::DeviceCert;

pub use tilo_peer_state::fixture::certify as device_with;

pub use tilo_peer_state::loopback::{addr, endpoint as localhost_endpoint};

/// The workspace `workspace`, made by `a` with `roster` in it, held as an
/// empty peer.
pub fn workspace(
    workspace: &str,
    a: &(SigningKey, DeviceCert),
    roster: &[DeviceCert],
) -> StatePeer {
    tilo_peer_state::fixture::Founding::by(a)
        .named(workspace)
        .with(roster)
        .at(tilo_clock::wall())
        .exact()
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
