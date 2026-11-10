//! Where a workspace's host is reached.

use iroh_base::{EndpointAddr, RelayUrl};

/// A workspace's host (`tilo-relay`), as a device reaches it.
#[derive(Clone, Debug)]
pub struct Host {
    /// Its iroh endpoint.
    pub addr: EndpointAddr,
    /// An iroh relay server to dial it through, if any.
    pub relay_url: Option<RelayUrl>,
}

impl Host {
    /// The host at `addr`, dialled through the relay `addr` names, if any.
    #[must_use]
    pub fn at(addr: EndpointAddr) -> Self {
        Self {
            addr,
            relay_url: None,
        }
    }
}
