//! Contract types of the shared workspace join: the signed address a device gives for a
//! workspace it serves, the trust it derives, the semantic service callback and its errors.

use serde::{Deserialize, Serialize};
use std::future::Future;
use tilo_relay_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;
use tilo_workspace_join::{
    JoinBootstrap, JoinTransportRequest, JoinTransportResponse,
    TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1, TransportHint, sign_transport_hint,
    verify_transport_hint,
};

/// Public Iroh ALPN for invitation redemption and membership refresh.
pub const WORKSPACE_JOIN_ALPN: &[u8] = b"tilo-workspace-join/1";
/// Largest accepted encoded request, including a signed invitation and certificate.
pub const MAX_WORKSPACE_JOIN_REQUEST_BYTES: usize = 256 * 1024;
/// Largest accepted bootstrap response containing governed workspace state.
pub const MAX_WORKSPACE_JOIN_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Proof that a device holds one workspace's keys and serves its join: the workspace, the
/// device that holds them, and the service key at its endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServedWorkspace {
    workspace_id: WorkspaceId,
    device: DeviceCert,
    service_public_key: [u8; 32],
}

/// Bootstrap paired with the proof of the workspace it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceJoinAdmission {
    /// Authoritative governed state committed before the device replied.
    pub bootstrap: JoinBootstrap,
    /// The workspace the bootstrap is scoped to, and the device that signed for it.
    pub served: ServedWorkspace,
}

impl ServedWorkspace {
    /// The workspace whose keys the device holds.
    #[must_use]
    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// The device that must answer for the workspace on the connection.
    #[must_use]
    pub fn device(&self) -> &DeviceCert {
        &self.device
    }

    /// Exact Iroh service key at the signed endpoint.
    #[must_use]
    pub fn service_public_key(&self) -> [u8; 32] {
        self.service_public_key
    }

    /// Verify that `device` may serve exactly `workspace_id` right now, and bind that to the
    /// Iroh service key at its endpoint.
    ///
    /// # Errors
    ///
    /// The certificate does not verify now, or the device may not govern workspaces.
    pub fn verify(
        workspace_id: WorkspaceId,
        device: DeviceCert,
        service_public_key: [u8; 32],
    ) -> Result<Self, WorkspaceJoinError> {
        tilo_user_identity::verify_device_cert_at(&device, tilo_clock::unix_now_ns())
            .map_err(|error| WorkspaceJoinError::Protocol(error.to_string()))?;
        if !device.capabilities.can_own_workspaces {
            return Err(WorkspaceJoinError::Protocol(
                "a device that serves a workspace cannot govern workspaces".into(),
            ));
        }
        Ok(Self {
            workspace_id,
            device,
            service_public_key,
        })
    }
}

/// Canonical payload for the signed Iroh address of a workspace's device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDeviceHintV1 {
    /// Frozen payload version.
    pub version: u32,
    /// Whether the device holds the workspace's keys: it reads its records. A device that
    /// says false keeps the workspace's ciphertext only.
    pub holds_keys: bool,
    /// Whether the device runs this workspace's work — its records' actions, their indexes —
    /// for the rest of the person's devices, as its work setting takes it.
    pub runs_work: bool,
    /// The workspace this endpoint serves.
    pub workspace_id: WorkspaceId,
    /// Exact Iroh service address the guest dials.
    pub endpoint_addr: iroh_base::EndpointAddr,
    /// Person root that certified the device.
    pub user_public_key: [u8; 32],
    /// Root-certified identity of the device that signed the service key binding.
    pub device_certificate: DeviceCert,
    /// HTTP packet relay used by this private/local Iroh deployment.
    pub packet_relay_url: String,
}

/// `endpoint` without the addresses only a network next to it can reach: what
/// an invitation, and every link made from one, may name.
///
/// A device reports every address it answers at, private and link-local ones
/// included, because that is how its own neighbours find it directly. A link
/// is not handed to a neighbour but to a stranger, who reads whatever it
/// carries: an invitation that named `10.75.1.15:7704` published the private
/// addressing of the network the device stands in, to everyone the link reached,
/// and a device that dialled it reached nothing. So a link carries the iroh
/// relay the device is reached through and the addresses anyone can dial
/// ([`reachable_at`]), and nothing else — an iroh relay of the device's own
/// network no more a stranger's than the addresses beside it
/// ([`reachable_relay`]).
///
/// A device with no such address at all — a desktop on a home network with no
/// relay, or one whose relay is a server of its own network — keeps the
/// addresses it has: it is the only way into the workspace, and whoever the
/// person hands the link to is on that network or gets nothing either way.
/// Nothing is published in that case that a stranger could use.
#[must_use]
pub fn reachable(endpoint: &iroh_base::EndpointAddr) -> iroh_base::EndpointAddr {
    let kept: std::collections::BTreeSet<iroh_base::TransportAddr> = endpoint
        .addrs
        .iter()
        .filter(|address| match address {
            iroh_base::TransportAddr::Ip(socket) => reachable_at(*socket),
            iroh_base::TransportAddr::Relay(url) => reachable_relay(url),
            // A transport this build does not know is not a network address to
            // judge.
            _ => true,
        })
        .cloned()
        .collect();
    if kept.is_empty() && !endpoint.addrs.is_empty() {
        return endpoint.clone();
    }
    iroh_base::EndpointAddr::from_parts(endpoint.id, kept)
}

/// Whether a stranger can reach the relay `url` names.
///
/// A relay is reached from anywhere only when it stands somewhere anyone can
/// dial: one listening on a loopback, private or link-local address is its
/// own network's way in, no more a stranger's than the device's own address
/// beside it. A name is whoever runs it to say where it resolves.
#[must_use]
pub fn reachable_relay(url: &iroh_base::RelayUrl) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };
    let bare = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    match bare.parse::<std::net::IpAddr>() {
        Ok(address) => reachable_ip(address),
        // A name stands for an address this link cannot know, and a relay
        // named by one is reached the way any other name is.
        Err(_) => true,
    }
}

/// Whether a stranger can dial `address`: whether it is a global unicast
/// address rather than one that only means something inside a network.
///
/// Kept out are the loopback, private and shared (carrier-grade NAT,
/// `100.64.0.0/10`), link-local, unspecified, multicast, broadcast,
/// documentation, benchmarking and reserved ranges of IPv4, and their IPv6
/// counterparts (loopback, `fc00::/7` unique-local, `fe80::/10` link-local,
/// unspecified, multicast), as well as an IPv4 address written as an IPv6 one
/// (`::ffff:10.0.0.1`).
#[must_use]
pub fn reachable_at(address: std::net::SocketAddr) -> bool {
    reachable_ip(address.ip()) && address.port() != 0
}

/// [`reachable_at`] of an address without one to dial it at.
fn reachable_ip(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(v4) => {
            let octets = v4.octets();
            !(v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                // 0.0.0.0/8: "this network" only.
                || octets[0] == 0
                // 100.64.0.0/10: shared address space, which is carrier-grade
                // NAT rather than anything a stranger reaches.
                || (octets[0] == 100 && (64..128).contains(&octets[1]))
                // 198.18.0.0/15: benchmarking networks.
                || (octets[0] == 198 && matches!(octets[1], 18 | 19))
                // 240.0.0.0/4: reserved, the broadcast address inside it too.
                || octets[0] >= 240)
        }
        std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => reachable_ip(v4.into()),
            None => {
                !(v6.is_unspecified()
                    || v6.is_loopback()
                    || v6.is_multicast()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local())
            }
        },
    }
}

impl WorkspaceDeviceHintV1 {
    /// Sign this opaque address payload and its distinct Iroh service key with the device's key.
    pub fn transport_hint(
        &self,
        device_key: &dyn collab_types::signer::DeviceSigner,
    ) -> Result<TransportHint, WorkspaceJoinError> {
        self.validate()?;
        sign_transport_hint(
            TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1,
            serde_json::to_string(self)
                .map_err(|error| WorkspaceJoinError::Encode(error.to_string()))?,
            self.device_certificate.clone(),
            *self.endpoint_addr.id.as_bytes(),
            device_key,
        )
        .map_err(WorkspaceJoinError::Protocol)
    }

    /// Parse and verify a signed Iroh address of a workspace's device.
    pub fn from_transport_hint(hint: &TransportHint) -> Result<Self, WorkspaceJoinError> {
        if hint.kind != TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1 {
            return Err(WorkspaceJoinError::Protocol(
                "transport hint is not an Iroh address of a workspace's device".into(),
            ));
        }
        let value: Self = serde_json::from_str(&hint.opaque_json)
            .map_err(|error| WorkspaceJoinError::Decode(error.to_string()))?;
        if hint.host != value.device_certificate {
            return Err(WorkspaceJoinError::Protocol(
                "workspace address does not match its signed device pin".into(),
            ));
        }
        verify_transport_hint(hint).map_err(WorkspaceJoinError::Protocol)?;
        if hint.service_public_key != *value.endpoint_addr.id.as_bytes() {
            return Err(WorkspaceJoinError::Protocol(
                "workspace service pin does not match its Iroh endpoint".into(),
            ));
        }
        value.validate()?;
        Ok(value)
    }

    /// Verify a signed hint and derive its proof without contacting the device.
    ///
    /// This is the restart-safe path for an already persisted `TransportHint`. The trust token
    /// cannot be derived from the public payload struct alone because this method first verifies
    /// the device signature and its exact service-key binding.
    pub fn authenticate_transport_hint(
        hint: &TransportHint,
    ) -> Result<(Self, ServedWorkspace), WorkspaceJoinError> {
        let device = Self::from_transport_hint(hint)?;
        let served = device.served();
        Ok((device, served))
    }

    /// Require an inspected invitation to name this exact workspace, issued by
    /// the device itself.
    pub fn validate_invitation(
        &self,
        grant: &tilo_workspace_join::InviteGrant,
    ) -> Result<ServedWorkspace, WorkspaceJoinError> {
        if grant.workspace_id != self.workspace_id {
            return Err(WorkspaceJoinError::Protocol(
                "invitation scope does not match its signed workspace address".into(),
            ));
        }
        if grant.issuer != self.device_certificate {
            return Err(WorkspaceJoinError::Protocol(
                "the device that serves the workspace did not issue the invitation".into(),
            ));
        }
        Ok(self.served())
    }

    /// Require a returned bootstrap to come from this signed device's scope.
    pub fn validate_bootstrap(
        &self,
        bootstrap: &JoinBootstrap,
    ) -> Result<ServedWorkspace, WorkspaceJoinError> {
        // The device that answered is the one the hint signs for.
        if (bootstrap.workspace_id, &bootstrap.host)
            != (self.workspace_id, &self.device_certificate)
        {
            return Err(WorkspaceJoinError::Protocol(
                "workspace bootstrap does not match its signed address".into(),
            ));
        }
        Ok(self.served())
    }

    fn served(&self) -> ServedWorkspace {
        ServedWorkspace {
            workspace_id: self.workspace_id,
            device: self.device_certificate.clone(),
            service_public_key: *self.endpoint_addr.id.as_bytes(),
        }
    }

    fn validate(&self) -> Result<(), WorkspaceJoinError> {
        if self.version != 1
            || self.workspace_id.is_nil()
            || self.device_certificate.user_public_key != self.user_public_key
        {
            return Err(WorkspaceJoinError::Protocol(
                "workspace address identity is inconsistent".into(),
            ));
        }
        let now_ns = tilo_clock::unix_now_ns();
        tilo_user_identity::verify_device_cert_at(&self.device_certificate, now_ns)
            .map_err(|error| WorkspaceJoinError::Protocol(error.to_string()))?;
        if self.holds_keys && !self.device_certificate.capabilities.can_own_workspaces {
            return Err(WorkspaceJoinError::Protocol(
                "a device that holds a workspace's keys cannot govern workspaces".into(),
            ));
        }
        if self.runs_work && !self.holds_keys {
            return Err(WorkspaceJoinError::Protocol(
                "a device that runs a workspace's work holds no keys for it".into(),
            ));
        }
        if self.packet_relay_url.is_empty() || self.packet_relay_url.len() > 4096 {
            return Err(WorkspaceJoinError::Protocol(
                "workspace packet relay URL is invalid".into(),
            ));
        }
        Ok(())
    }
}

/// Serialized semantic callback implemented by each workspace authority store.
pub trait WorkspaceJoinService: Clone + Send + Sync + std::fmt::Debug + 'static {
    /// Handle one typed request bound to the authenticated QUIC endpoint key.
    fn handle(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
    ) -> impl Future<Output = JoinTransportResponse> + Send;
}

/// Failures before or returned by the semantic workspace authority.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceJoinError {
    /// Iroh or stream failure.
    #[error("workspace join transport failed: {0}")]
    Transport(String),
    /// The bounded exchange deadline elapsed.
    #[error("workspace join request timed out")]
    Timeout,
    /// Local request serialization failed.
    #[error("workspace join request encoding failed: {0}")]
    Encode(String),
    /// Remote response decoding failed.
    #[error("workspace join response decoding failed: {0}")]
    Decode(String),
    /// The typed request or response violated the protocol contract.
    #[error("workspace join protocol rejected the exchange: {0}")]
    Protocol(String),
    /// The authoritative service rejected a well-formed request.
    #[error("the device that serves the workspace rejected the request ({code}): {message}")]
    Remote {
        /// Stable HTTP-like status code used across transports.
        code: u16,
        /// Public-safe rejection reason.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a failed step in a test is the assertion"
    )]

    use super::*;
    use iroh_base::{EndpointAddr, TransportAddr};
    use std::net::SocketAddr;

    fn at(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    fn with(addrs: &[&str]) -> EndpointAddr {
        EndpointAddr::from_parts(
            iroh_base::SecretKey::from_bytes(&[7; 32]).public(),
            addrs.iter().map(|text| match text.parse::<SocketAddr>() {
                Ok(socket) => TransportAddr::Ip(socket),
                Err(_) => TransportAddr::Relay(text.parse().unwrap()),
            }),
        )
    }

    #[test]
    fn an_invitation_names_the_relay_and_no_address_of_the_network_behind_it() {
        // The address a live relay reported beside its own: the pod's network.
        let reported = with(&[
            "10.75.1.15:7704",
            "100.64.3.4:7704",
            "127.0.0.1:7704",
            "169.254.7.7:7704",
            "192.168.1.9:7704",
            "172.20.0.2:7704",
            "[::1]:7704",
            "[fe80::1]:7704",
            "[fd12:3456::1]:7704",
            "[::ffff:10.0.0.1]:7704",
            "0.0.0.0:7704",
            "[::]:7704",
            "https://relay.example./",
        ]);
        let named = reachable(&reported);
        assert_eq!(named.id, reported.id, "a link still names the endpoint");
        assert_eq!(
            named.addrs,
            with(&["https://relay.example./"]).addrs,
            "only the relay is left: {:?}",
            named.addrs
        );
    }

    #[test]
    fn a_public_address_is_kept_beside_the_relay() {
        let named = reachable(&with(&[
            "93.184.216.34:7704",
            "10.0.0.4:7704",
            "203.0.113.4:7704",
            "[2606:2800:220:1::1]:7704",
            "[fd00::4]:7704",
            "https://relay.example./",
        ]));
        assert_eq!(
            named.addrs,
            with(&[
                "93.184.216.34:7704",
                "[2606:2800:220:1::1]:7704",
                "https://relay.example./",
            ])
            .addrs,
            "a stranger dials the public addresses and the relay, and nothing else: {:?}",
            named.addrs
        );
    }

    #[test]
    fn a_device_only_its_own_network_can_reach_still_names_it() {
        // A desktop at home with no relay: its neighbours' addresses are the
        // only way in, and a link that named none would open nothing.
        let home = with(&["192.168.1.9:7704", "[fe80::9]:7704"]);
        assert_eq!(reachable(&home), home);
        let alone = with(&[]);
        assert_eq!(reachable(&alone), alone, "an endpoint names what it has");
    }

    #[test]
    fn a_relay_inside_the_network_is_no_strangers_way_in() {
        // A device running its own packet relay on the network it stands in:
        // the relay's address is that network's, so a link carrying only it
        // names nothing a device elsewhere can dial, and it keeps the
        // addresses beside it that its neighbours read.
        let inside = with(&[
            "http://127.0.0.1:37975/",
            "http://10.244.1.179:37975/",
            "10.244.1.179:54307",
        ]);
        assert_eq!(reachable(&inside), inside, "{:?}", reachable(&inside));
        // Reached through a relay anyone can dial, the same device's link names
        // that relay and drops the network it stands in.
        let outside = with(&["https://relay.example./", "10.244.1.179:54307"]);
        assert_eq!(
            reachable(&outside).addrs,
            with(&["https://relay.example./"]).addrs
        );
    }

    #[test]
    fn a_stranger_can_dial_no_relay_their_network_alone_reaches() {
        for url in [
            "http://127.0.0.1:7704/",
            "http://[::1]:7704/",
            "http://10.75.1.15:7704/",
            "http://192.168.1.9:7704/",
            "http://[fd12:3456::1]:7704/",
        ] {
            assert!(!reachable_relay(&url.parse().unwrap()), "{url}");
        }
        for url in [
            "https://relay.example./",
            "https://relay.tilo.so/",
            "https://93.184.216.34:7704/",
            "https://[2606:2800:220:1::1]/",
        ] {
            assert!(reachable_relay(&url.parse().unwrap()), "{url}");
        }
    }

    #[test]
    fn a_stranger_can_dial_neither_a_private_nor_a_loopback_address() {
        for address in [
            "127.0.0.1:7704",
            "10.75.1.15:7704",
            "100.64.0.1:7704",
            "169.254.1.1:7704",
            "192.168.0.1:7704",
            "172.16.0.1:7704",
            "0.0.0.0:7704",
            "255.255.255.255:7704",
            "[::1]:7704",
            "[fe80::1]:7704",
            "[fc00::1]:7704",
            "[::]:7704",
            "[::ffff:192.168.0.1]:7704",
            "203.0.113.1:7704",
        ] {
            assert!(!reachable_at(at(address)), "{address}");
        }
        for address in [
            "93.184.216.34:7704",
            "[2606:2800:220:1::1]:7704",
            "8.8.8.8:1",
        ] {
            assert!(reachable_at(at(address)), "{address}");
        }
        assert!(
            !reachable_at(at("93.184.216.34:0")),
            "a port nothing listens at is no address to name"
        );
    }
}
