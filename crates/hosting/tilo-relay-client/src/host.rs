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
        // A browser reaches a host only through a relay: one the address names
        // is the one its endpoint is bound to, or it has no transport at all.
        let relay_url = addr.relay_urls().next().cloned();
        Self { addr, relay_url }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a test's parse is its assertion")]

    use super::Host;
    use iroh_base::{EndpointAddr, RelayUrl, SecretKey};

    #[test]
    fn a_host_is_dialled_through_the_relay_its_address_names() {
        let key = SecretKey::from_bytes(&[7; 32]).public();
        let relay: RelayUrl = "https://relay.example".parse().unwrap();
        let named = Host::at(EndpointAddr::new(key).with_relay_url(relay.clone()));
        assert_eq!(named.relay_url, Some(relay));
        assert_eq!(Host::at(EndpointAddr::new(key)).relay_url, None);
    }
}
