//! Contract types of the shared workspace join: the signed Iroh host hint, the relay trust
//! it derives, the semantic service callback and its errors.

use serde::{Deserialize, Serialize};
use std::future::Future;
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

/// Canonical payload for the signed Iroh workspace-host transport hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceHostMode {
    /// Dedicated home/cloud host certified with `can_act_as_host`.
    CertifiedHost,
    /// One-workspace native owner whose own device terminates service.
    NativeInteractiveWorkspaceOwner,
}

/// Verified relay-authentication scope derived only from the signed hint.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "public trust mode chosen once per join; boxing would change its constructors in dependent crates"
)]
pub enum WorkspaceHostTrust {
    /// Existing dedicated-host authentication remains capability-gated.
    CertifiedHost,
    /// Narrow exception for an owner device serving its own workspace.
    InteractiveOwner(InteractiveOwnerTrust),
}

/// Opaque proof that a primary device may host one exact governed workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractiveOwnerTrust {
    room: String,
    workspace_id: u64,
    host: DeviceCert,
    service_public_key: [u8; 32],
}

/// Bootstrap paired with the verified relay-authentication scope that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceJoinAdmission {
    /// Authoritative governed state committed before the host replied.
    pub bootstrap: JoinBootstrap,
    /// Trust mode and exact scope required for the subsequent relay connection.
    pub trust: WorkspaceHostTrust,
}

impl InteractiveOwnerTrust {
    /// Canonical workspace UUID this exception covers.
    pub fn room(&self) -> &str {
        &self.room
    }

    /// Exact workspace ID this exception covers.
    pub fn workspace_id(&self) -> u64 {
        self.workspace_id
    }

    /// The owner device that must answer relay authentication.
    pub fn host(&self) -> &DeviceCert {
        &self.host
    }

    /// Exact Iroh service key terminating the signed endpoint.
    pub fn service_public_key(&self) -> [u8; 32] {
        self.service_public_key
    }

    /// Verify that `host` may serve exactly `workspace`/`workspace_id` right now, and bind
    /// that scope to the Iroh service key terminating it.
    pub fn verify(
        room: String,
        workspace_id: u64,
        host: DeviceCert,
        service_public_key: [u8; 32],
    ) -> Result<Self, WorkspaceJoinError> {
        let canonical_room = uuid::Uuid::parse_str(&room)
            .map(|room| room.to_string())
            .map_err(|_| {
                WorkspaceJoinError::Protocol("interactive owner room is invalid".into())
            })?;
        if canonical_room != room
            || !tilo_relay_protocol::room_workspace_id_matches(&room, workspace_id)
        {
            return Err(WorkspaceJoinError::Protocol(
                "interactive owner room/workspace binding is invalid".into(),
            ));
        }
        tilo_user_identity::verify_device_cert_at(&host, tilo_clock::unix_now_ns())
            .map_err(|error| WorkspaceJoinError::Protocol(error.to_string()))?;
        if !host.capabilities.can_own_workspaces {
            return Err(WorkspaceJoinError::Protocol(
                "interactive owner certificate cannot govern workspaces".into(),
            ));
        }
        Ok(InteractiveOwnerTrust {
            room,
            workspace_id,
            host,
            service_public_key,
        })
    }
}

/// Canonical payload for the signed Iroh workspace-host transport hint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IrohWorkspaceHostHintV1 {
    /// Frozen payload version.
    pub version: u32,
    /// Explicit trust mode; never inferred from an unsigned local setting.
    pub mode: WorkspaceHostMode,
    /// Canonical governed workspace UUID served by this endpoint.
    pub room: String,
    /// Exact governed workspace ID served by this endpoint.
    pub workspace_id: u64,
    /// Exact Iroh service address the guest dials.
    pub endpoint_addr: iroh_base::EndpointAddr,
    /// Person root that certified the host device.
    pub host_user_public_key: [u8; 32],
    /// Root-certified host identity that signed the service key binding.
    pub host_device_certificate: DeviceCert,
    /// HTTP packet relay used by this private/local Iroh deployment.
    pub packet_relay_url: String,
}

/// `endpoint` without the addresses only a network next to it can reach: what
/// an invitation, and every link made from one, may name.
///
/// A host reports every address it answers at, private and link-local ones
/// included, because that is how its own neighbours find it directly. A link
/// is not handed to a neighbour but to a stranger, who reads whatever it
/// carries: an invitation that named `10.75.1.15:7704` published the private
/// addressing of the network the host stands in, to everyone the link reached,
/// and a device that dialled it reached nothing. So a link carries the relay
/// the host is reached through and the addresses anyone can dial
/// ([`reachable_at`]), and nothing else — a relay of the host's own network
/// no more a stranger's than the addresses beside it ([`reachable_relay`]).
///
/// A host with no such address at all — a desktop on a home network with no
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
/// dial: one listening on a loopback, private or link-local address is the
/// host's own network's way in, no more a stranger's than the host's own
/// address beside it. A name is whoever runs it to say where it resolves.
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

/// A signed hint as an earlier build wrote it: it also named the device
/// that governed the workspace then. Restarts still read hints their devices
/// pinned, which is written beside the workspaces they hold.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredHint {
    version: u32,
    mode: WorkspaceHostMode,
    room: String,
    workspace_id: u64,
    endpoint_addr: iroh_base::EndpointAddr,
    host_user_public_key: [u8; 32],
    host_device_certificate: DeviceCert,
    packet_relay_url: String,
    /// The workspace's governor in a hint from before it named its host alone:
    /// read, never written.
    #[serde(default, rename = "governing_authority")]
    _governing_authority: Option<DeviceCert>,
}

impl<'de> Deserialize<'de> for IrohWorkspaceHostHintV1 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = StoredHint::deserialize(deserializer)?;
        Ok(Self {
            version: stored.version,
            mode: stored.mode,
            room: stored.room,
            workspace_id: stored.workspace_id,
            endpoint_addr: stored.endpoint_addr,
            host_user_public_key: stored.host_user_public_key,
            host_device_certificate: stored.host_device_certificate,
            packet_relay_url: stored.packet_relay_url,
        })
    }
}

impl IrohWorkspaceHostHintV1 {
    /// Sign this opaque address payload and its distinct Iroh service key with the host device.
    pub fn transport_hint(
        &self,
        host_key: &ed25519_dalek::SigningKey,
    ) -> Result<TransportHint, WorkspaceJoinError> {
        self.validate()?;
        sign_transport_hint(
            TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1,
            serde_json::to_string(self)
                .map_err(|error| WorkspaceJoinError::Encode(error.to_string()))?,
            self.host_device_certificate.clone(),
            *self.endpoint_addr.id.as_bytes(),
            host_key,
        )
        .map_err(WorkspaceJoinError::Protocol)
    }

    /// Parse and verify a signed Iroh workspace-host hint.
    pub fn from_transport_hint(hint: &TransportHint) -> Result<Self, WorkspaceJoinError> {
        if hint.kind != TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1 {
            return Err(WorkspaceJoinError::Protocol(
                "transport hint is not an Iroh workspace host".into(),
            ));
        }
        let value: Self = serde_json::from_str(&hint.opaque_json)
            .map_err(|error| WorkspaceJoinError::Decode(error.to_string()))?;
        if hint.host != value.host_device_certificate {
            return Err(WorkspaceJoinError::Protocol(
                "workspace-host hint payload does not match its signed host pin".into(),
            ));
        }
        verify_transport_hint(hint).map_err(WorkspaceJoinError::Protocol)?;
        if hint.service_public_key != *value.endpoint_addr.id.as_bytes() {
            return Err(WorkspaceJoinError::Protocol(
                "workspace-host service pin does not match its Iroh endpoint".into(),
            ));
        }
        value.validate()?;
        Ok(value)
    }

    /// Verify a signed hint and derive its opaque relay trust without contacting the host.
    ///
    /// This is the restart-safe path for an already persisted `TransportHint`. The trust token
    /// cannot be derived from the public payload struct alone because this method first verifies
    /// the host signature and its exact service-key binding.
    pub fn authenticate_transport_hint(
        hint: &TransportHint,
    ) -> Result<(Self, WorkspaceHostTrust), WorkspaceJoinError> {
        let host = Self::from_transport_hint(hint)?;
        let trust = host.verified_trust();
        Ok((host, trust))
    }

    /// Require an inspected invitation to name this exact host scope, issued
    /// by the host itself.
    pub fn validate_invitation(
        &self,
        grant: &tilo_workspace_join::InviteGrant,
    ) -> Result<WorkspaceHostTrust, WorkspaceJoinError> {
        if grant.room != self.room || grant.workspace_id != self.workspace_id {
            return Err(WorkspaceJoinError::Protocol(
                "invitation scope does not match its signed workspace host hint".into(),
            ));
        }
        if grant.issuer != self.host_device_certificate {
            return Err(WorkspaceJoinError::Protocol(
                "the workspace host did not issue the invitation".into(),
            ));
        }
        Ok(self.verified_trust())
    }

    /// Require a returned bootstrap to come from this signed host's scope.
    pub fn validate_bootstrap(
        &self,
        bootstrap: &JoinBootstrap,
    ) -> Result<WorkspaceHostTrust, WorkspaceJoinError> {
        // The host that answered is the one the hint signs for.
        if (&bootstrap.room, bootstrap.workspace_id, &bootstrap.host)
            != (&self.room, self.workspace_id, &self.host_device_certificate)
        {
            return Err(WorkspaceJoinError::Protocol(
                "workspace bootstrap does not match its signed host hint".into(),
            ));
        }
        Ok(self.verified_trust())
    }

    fn verified_trust(&self) -> WorkspaceHostTrust {
        match self.mode {
            WorkspaceHostMode::CertifiedHost => WorkspaceHostTrust::CertifiedHost,
            WorkspaceHostMode::NativeInteractiveWorkspaceOwner => {
                WorkspaceHostTrust::InteractiveOwner(InteractiveOwnerTrust {
                    room: self.room.clone(),
                    workspace_id: self.workspace_id,
                    host: self.host_device_certificate.clone(),
                    service_public_key: *self.endpoint_addr.id.as_bytes(),
                })
            }
        }
    }

    fn validate(&self) -> Result<(), WorkspaceJoinError> {
        let canonical_room = uuid::Uuid::parse_str(&self.room)
            .map(|room| room.to_string())
            .map_err(|_| WorkspaceJoinError::Protocol("workspace host room is invalid".into()))?;
        if self.version != 1
            || canonical_room != self.room
            || !tilo_relay_protocol::room_workspace_id_matches(&self.room, self.workspace_id)
            || self.host_device_certificate.user_public_key != self.host_user_public_key
        {
            return Err(WorkspaceJoinError::Protocol(
                "workspace-host transport hint identity is inconsistent".into(),
            ));
        }
        let now_ns = tilo_clock::unix_now_ns();
        tilo_user_identity::verify_device_cert_at(&self.host_device_certificate, now_ns)
            .map_err(|error| WorkspaceJoinError::Protocol(error.to_string()))?;
        match self.mode {
            WorkspaceHostMode::CertifiedHost
                if !self.host_device_certificate.capabilities.can_act_as_host =>
            {
                return Err(WorkspaceJoinError::Protocol(
                    "certified workspace host lacks host capability".into(),
                ));
            }
            WorkspaceHostMode::NativeInteractiveWorkspaceOwner
                if !self.host_device_certificate.capabilities.can_own_workspaces =>
            {
                return Err(WorkspaceJoinError::Protocol(
                    "interactive workspace host cannot own workspaces".into(),
                ));
            }
            _ => {}
        }
        if self.packet_relay_url.is_empty() || self.packet_relay_url.len() > 4096 {
            return Err(WorkspaceJoinError::Protocol(
                "workspace-host packet relay URL is invalid".into(),
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
    #[error("workspace join host rejected the request ({code}): {message}")]
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
    fn a_host_only_its_own_network_can_reach_still_names_it() {
        // A desktop at home with no relay: its neighbours' addresses are the
        // only way in, and a link that named none would open nothing.
        let home = with(&["192.168.1.9:7704", "[fe80::9]:7704"]);
        assert_eq!(reachable(&home), home);
        let alone = with(&[]);
        assert_eq!(reachable(&alone), alone, "an endpoint names what it has");
    }

    #[test]
    fn a_relay_inside_the_network_is_no_strangers_way_in() {
        // A host running its own packet relay on the network it stands in:
        // the relay's address is that network's, so a link carrying only it
        // names nothing a device elsewhere can dial, and it keeps the
        // addresses beside it that its neighbours read.
        let inside = with(&[
            "http://127.0.0.1:37975/",
            "http://10.244.1.179:37975/",
            "10.244.1.179:54307",
        ]);
        assert_eq!(reachable(&inside), inside, "{:?}", reachable(&inside));
        // Reached through a relay anyone can dial, the same host's link names
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
