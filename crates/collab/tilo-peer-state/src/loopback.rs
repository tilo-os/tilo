//! The endpoints tests link devices over: loopback only, no relay, and no
//! portmapper, whose `UPnP` probe and name lookups a test sandbox refuses.
#![allow(
    clippy::missing_panics_doc,
    clippy::unwrap_used,
    reason = "test fixtures: a failed step is the test failing"
)]

use ed25519_dalek::SigningKey;
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use std::net::{Ipv4Addr, SocketAddr};

/// A loopback endpoint whose identity is the device `key`.
pub async fn endpoint(key: &SigningKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

/// Where `endpoint` is dialled.
pub fn addr(endpoint: &Endpoint) -> EndpointAddr {
    let port = endpoint.bound_sockets().first().unwrap().port();
    EndpointAddr::new(endpoint.id()).with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}
