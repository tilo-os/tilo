//! What the relay tests share: the workspace, its keys, and the endpoints and grants a
//! session dials and joins with.

#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "each test binary uses some of these, and a failed step is the test failing"
)]

use std::net::{Ipv4Addr, SocketAddr};

use iroh::{Endpoint, EndpointAddr, RelayMode};
use key_group::{ContentKeys, KeyGroup, MemberKey};
use tilo_live::live::{self, Track};
use tilo_live::live_session::{Heard, LiveSession};
use tilo_live::scope::Scope;
use tilo_live::{grant, native};

pub const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

pub fn scope() -> Scope {
    Scope::workspace(WORKSPACE)
}

pub fn keys() -> ContentKeys {
    let workspace = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
    KeyGroup::create(workspace, MemberKey::generate("owner").unwrap())
        .unwrap()
        .content_keys()
        .unwrap()
}

pub async fn endpoint() -> Endpoint {
    Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .transport_config(native::transport_config())
        .bind()
        .await
        .unwrap()
}

pub fn addr(endpoint: &Endpoint) -> EndpointAddr {
    let port = endpoint.bound_sockets().first().unwrap().port();
    EndpointAddr::new(endpoint.id()).with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

pub fn grant_for(certificate: &[u8]) -> Vec<u8> {
    grant::Native::new(
        live::workspace_bytes(WORKSPACE).unwrap(),
        &scope().id(),
        1,
        certificate.to_vec(),
        Vec::new(),
    )
    .unwrap()
    .encode()
}

pub fn heard(live_session: &LiveSession, track: Track) -> Vec<Heard> {
    let mut bodies = Vec::new();
    while let Some(heard) = live_session.try_recv() {
        if heard.track == track {
            bodies.push(heard);
        }
    }
    bodies
}
