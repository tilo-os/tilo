//! A hosted VM's network: the public internet, nothing of the host's. Each VM gets
//! a network namespace of its own bridged to `pasta`, a bridge filter that drops
//! private, link-local, reserved and metadata destinations, the server as its one
//! other endpoint, and Firecracker's bandwidth limiters. Setting this up needs
//! `CAP_NET_ADMIN` and `CAP_SYS_ADMIN`, so [`super::jail`] does it in the
//! privileged helper before it starts the jailer.

use std::net::{Ipv4Addr, SocketAddr};

use serde_json::json;

/// The VM's address, the same in every VM so a snapshot restores on any host.
pub const GUEST_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
/// Its gateway: `pasta`.
pub const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
/// Its resolver: `pasta`'s forwarder to the host's chosen resolver.
pub const DNS: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 3);
/// Where the VM reaches the host's own loopback, when the server is there (a local stack): `pasta`
/// maps this address to it, and the filter allows it. A host neither binds nor routes it.
pub const HOST_LOOPBACK: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 4);
pub const PREFIX: u8 = 24;
/// The VM's MAC address, fixed so a restored VM's ARP entries hold.
pub const GUEST_MAC: &str = "06:00:0a:00:02:0f";
pub const TAP: &str = "tap0";
pub const UPLINK: &str = "pt0";
pub const BRIDGE: &str = "br0";
/// Frames no larger than the VM's own: `pasta` would otherwise hand the
/// bridge 64 KiB frames the VM's tap drops.
pub const MTU: u16 = 1500;

/// Networks no VM reaches, whatever the host adds.
pub const ALWAYS_BLOCKED: &[(Ipv4Addr, u8)] = &[
    (Ipv4Addr::UNSPECIFIED, 8),
    // Private networks: the host's, the cluster's nodes, pods and services.
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    // Shared address space (carrier-grade NAT, some cluster networks).
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    // Link-local: the cloud metadata endpoint 169.254.169.254 among them.
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    // Azure's platform address (wire server, health probes, Azure DNS).
    (Ipv4Addr::new(168, 63, 129, 16), 32),
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    // Multicast, reserved and broadcast.
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    (Ipv4Addr::new(240, 0, 0, 0), 4),
];

/// `ALWAYS_BLOCKED` and the host's own (its public addresses, a cluster
/// network outside the private ranges).
#[must_use]
pub fn blocked_networks(extra: &[(Ipv4Addr, u8)]) -> Vec<(Ipv4Addr, u8)> {
    let mut all = ALWAYS_BLOCKED.to_vec();
    for network in extra {
        if !all.contains(network) {
            all.push(*network);
        }
    }
    all
}

/// The one destination a VM reaches besides the internet: the server its browser session joins the
/// workspace through, as the host reaches it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Endpoint {
    pub address: Ipv4Addr,
    pub port: u16,
}

impl Endpoint {
    /// `ADDR:PORT`, with the address written as four numbers: a host that names its server
    /// resolves nothing.
    ///
    /// # Errors
    ///
    /// The words saying what this should be.
    pub fn parse(text: &str) -> Result<Self, String> {
        let wrong = || format!("{text:?} is not ADDR:PORT, with ADDR an IPv4 address");
        let (address, port) = text.trim().split_once(':').ok_or_else(wrong)?;
        let address: Ipv4Addr = address.parse().map_err(|_| wrong())?;
        let port: u16 = port
            .parse()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(wrong)?;
        Ok(Self { address, port })
    }

    /// Where a VM reaches this endpoint: the same address, unless the server is on the host's own
    /// loopback (a local stack), where `pasta` maps [`HOST_LOOPBACK`] to it.
    #[must_use]
    pub fn guest(self) -> Self {
        if self.address.is_loopback() {
            Self {
                address: HOST_LOOPBACK,
                port: self.port,
            }
        } else {
            self
        }
    }

    #[must_use]
    pub fn on_host_loopback(self) -> bool {
        self.address.is_loopback()
    }

    #[must_use]
    pub fn socket(self) -> SocketAddr {
        SocketAddr::from((self.address, self.port))
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.address, self.port)
    }
}

/// `a.b.c.d/len`.
pub fn parse_network(text: &str) -> Result<(Ipv4Addr, u8), String> {
    let (address, prefix) = text.split_once('/').unwrap_or((text, "32"));
    let address: Ipv4Addr = address
        .parse()
        .map_err(|_| format!("{text:?} is not an IPv4 network"))?;
    let prefix: u8 = prefix
        .parse()
        .ok()
        .filter(|prefix| *prefix <= 32)
        .ok_or_else(|| format!("{text:?} has no prefix length 0..=32"))?;
    Ok((address, prefix))
}

#[cfg(target_os = "linux")]
#[must_use]
pub fn guest_filter(
    extra: &[(Ipv4Addr, u8)],
    endpoint: Option<Endpoint>,
) -> tilo_egress::nft::GuestFilter {
    tilo_egress::nft::GuestFilter {
        guest: TAP.to_string(),
        uplink: UPLINK.to_string(),
        dns: DNS,
        endpoint: endpoint.map(|endpoint| {
            let guest = endpoint.guest();
            (guest.address, guest.port)
        }),
        blocked: blocked_networks(extra),
    }
}

/// The kernel's own IP configuration for the VM (`CONFIG_IP_PNP`), with the resolver it reports in
/// `/proc/net/pnp`.
#[must_use]
pub fn kernel_ip_arg() -> String {
    let mask = Ipv4Addr::from(
        u32::MAX
            .checked_shl(32_u32.saturating_sub(u32::from(PREFIX)))
            .unwrap_or(0),
    );
    format!("ip={GUEST_ADDRESS}::{GATEWAY}:{mask}::eth0:off:{DNS}")
}

/// `pasta` for the namespace process `holder` is in, writing its pid to `pid_file` once the
/// namespace is connected and sending DNS to `resolver`. Nothing is forwarded inwards, no host
/// loopback is mapped unless `endpoint` is on it, IPv4 only.
#[must_use]
pub fn pasta_args(
    holder: u32,
    pid_file: &str,
    resolver: Ipv4Addr,
    endpoint: Option<Endpoint>,
) -> Vec<String> {
    let loopback = match endpoint {
        Some(endpoint) if endpoint.on_host_loopback() => HOST_LOOPBACK.to_string(),
        _ => "none".to_string(),
    };
    let args: Vec<String> = [
        "--quiet",
        "--netns",
        &format!("/proc/{holder}/ns/net"),
        "--userns",
        &format!("/proc/{holder}/ns/user"),
        "--ns-ifname",
        UPLINK,
        "--address",
        &GUEST_ADDRESS.to_string(),
        "--netmask",
        &PREFIX.to_string(),
        "--gateway",
        &GATEWAY.to_string(),
        "--dns-forward",
        &DNS.to_string(),
        "--dns-host",
        &resolver.to_string(),
        "--mtu",
        &MTU.to_string(),
        "--map-host-loopback",
        &loopback,
        "--ipv4-only",
        "--no-dhcp",
        "-t",
        "none",
        "-u",
        "none",
        "-T",
        "none",
        "-U",
        "none",
        "--pid",
        pid_file,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    args
}

/// Firecracker's network device for a VM limited to `mbit` megabits per second each way.
#[must_use]
pub fn interface_json(mbit: u32) -> serde_json::Value {
    // Tokens refill every 100 ms: a tenth of a second's bytes per bucket.
    let bucket = u64::from(mbit).saturating_mul(125_000) / 10;
    let limiter = json!({ "bandwidth": { "size": bucket.max(1500), "refill_time": 100 } });
    json!({
        "iface_id": "eth0",
        "host_dev_name": TAP,
        "guest_mac": GUEST_MAC,
        "rx_rate_limiter": limiter,
        "tx_rate_limiter": limiter,
    })
}
