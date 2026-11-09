//! A hosted VM's network: the public internet, nothing of the host's.
//!
//! Agents in a VM need `cargo`, `npm`, `pip` and `git` to reach the
//! internet; nothing in the VM may reach the host, the cluster around it,
//! the cloud's metadata endpoints or another tenant's VM. Each VM gets:
//!
//! ```text
//!  VM ── eth0 ═ tap0 ─┐                     VM's network namespace
//!                     br0 ── nftables bridge filter (destinations)
//!         pasta ═ pt0 ┘
//!           │  re-makes each connection from a socket of the host's
//!           ▼
//!     the host's network (in Kubernetes: the pod's, under its policy)
//! ```
//!
//! - **Its own network namespace**, holding Firecracker's tap (`tap0`) and
//!   `pasta`'s tap (`pt0`), bridged. The VM addresses are the same in every
//!   VM (`10.0.2.15`, gateway `10.0.2.2`, DNS `10.0.2.3`), so a snapshot
//!   restores on any host. The namespace belongs to a user namespace owned
//!   by the VM's *network* uid, which `pasta` runs as; the VM's own uid has
//!   no capability over it, so an escape from Firecracker cannot change the
//!   filter.
//! - **`pasta`** (the passt project; what rootless podman uses) as the only
//!   route out: it terminates the VM's TCP and UDP and opens a socket of
//!   the host's for each. It forwards nothing inwards and maps no host
//!   loopback. Nothing is routed or address-translated on the host, so the host needs no
//!   forwarding, no `iptables` rules of its own, and in Kubernetes the pod's
//!   network policy applies to VM traffic as to the pod's own.
//! - **A bridge filter** ([`tilo_egress::nft::GuestFilter`]) on `br0`: ARP
//!   and IPv4 to public destinations pass, DNS to `pasta`'s forwarder
//!   passes, and private, shared, link-local (the metadata endpoint
//!   `169.254.169.254`), loopback, multicast and reserved networks, Azure's
//!   platform address `168.63.129.16`, and whatever the host adds
//!   ([`blocked_networks`]), are dropped. IPv6 is off.
//! - **Bandwidth limits** in Firecracker's own rate limiters on the
//!   VM's network device, per direction ([`interface_json`]).
//!
//! Setting this up takes `CAP_NET_ADMIN` and `CAP_SYS_ADMIN`, which the
//! service does not have: [`super::jail`] does it in the privileged helper
//! before it starts the jailer.

use std::net::Ipv4Addr;

use serde_json::json;

/// The VM's address on its network.
pub const GUEST_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
/// Its gateway: `pasta`.
pub const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
/// Its resolver: `pasta`'s forwarder to the host's chosen resolver.
pub const DNS: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 3);
pub const PREFIX: u8 = 24;
/// The VM's MAC address, fixed so a restored VM's ARP entries hold.
pub const GUEST_MAC: &str = "06:00:0a:00:02:0f";
/// Firecracker's tap in the VM's namespace.
pub const TAP: &str = "tap0";
/// `pasta`'s tap in the VM's namespace.
pub const UPLINK: &str = "pt0";
/// The bridge joining the two.
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
/// The bridge filter of every VM on a host blocking `extra` besides
/// [`ALWAYS_BLOCKED`].
#[must_use]
pub fn guest_filter(extra: &[(Ipv4Addr, u8)]) -> tilo_egress::nft::GuestFilter {
    tilo_egress::nft::GuestFilter {
        guest: TAP.to_string(),
        uplink: UPLINK.to_string(),
        dns: DNS,
        blocked: blocked_networks(extra),
    }
}

/// The kernel's own IP configuration for the VM (`CONFIG_IP_PNP`), with the
/// resolver it reports in `/proc/net/pnp`.
#[must_use]
pub fn kernel_ip_arg() -> String {
    let mask = Ipv4Addr::from(
        u32::MAX
            .checked_shl(32_u32.saturating_sub(u32::from(PREFIX)))
            .unwrap_or(0),
    );
    format!("ip={GUEST_ADDRESS}::{GATEWAY}:{mask}::eth0:off:{DNS}")
}

/// `pasta` for the namespace process `holder` is in, writing its pid to
/// `pid_file` once the namespace is connected, and sending DNS to
/// `resolver`. Nothing is forwarded inwards, no host loopback is mapped,
/// IPv4 only.
#[must_use]
pub fn pasta_args(holder: u32, pid_file: &str, resolver: Ipv4Addr) -> Vec<String> {
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
        "none",
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

/// Firecracker's network device for a VM limited to `mbit` megabits per
/// second each way.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_metadata_endpoint_the_host_and_the_cluster_are_blocked() {
        let blocked = blocked_networks(&[parse_network("203.0.113.7").unwrap()]);
        let inside = |address: Ipv4Addr| {
            blocked.iter().any(|(network, prefix)| {
                let mask = u32::MAX.checked_shl(32 - u32::from(*prefix)).unwrap_or(0);
                u32::from(address) & mask == u32::from(*network) & mask
            })
        };
        for address in [
            "169.254.169.254",
            "168.63.129.16",
            "10.0.2.2",
            "10.244.1.5",
            "172.17.0.1",
            "192.168.1.1",
            "100.100.100.100",
            "127.0.0.1",
            "203.0.113.7",
        ] {
            assert!(inside(address.parse().unwrap()), "{address}");
        }
        for address in ["1.1.1.1", "151.101.1.137", "140.82.112.4"] {
            assert!(!inside(address.parse().unwrap()), "{address}");
        }
        assert_eq!(parse_network("10.1.0.0/16").unwrap().1, 16);
        assert!(parse_network("10.1.0.0/33").is_err());
        assert!(parse_network("fe80::1").is_err());
    }

    #[test]
    fn the_vm_configures_itself_and_pasta_forwards_nothing_inwards() {
        assert_eq!(
            kernel_ip_arg(),
            "ip=10.0.2.15::10.0.2.2:255.255.255.0::eth0:off:10.0.2.3"
        );
        let args = pasta_args(42, "/j/net/vm-3/pasta.pid", Ipv4Addr::new(1, 1, 1, 1));
        let after = |flag: &str| {
            args.iter()
                .position(|arg| arg == flag)
                .and_then(|at| args.get(at + 1))
                .map(String::as_str)
        };
        assert_eq!(after("--netns"), Some("/proc/42/ns/net"));
        assert_eq!(after("--map-host-loopback"), Some("none"));
        assert_eq!(after("--dns-host"), Some("1.1.1.1"));
        assert_eq!(after("--mtu"), Some("1500"));
        for flag in ["-t", "-u", "-T", "-U"] {
            assert_eq!(after(flag), Some("none"), "{flag}");
        }
        let interface = interface_json(200);
        assert_eq!(interface["host_dev_name"], TAP);
        assert_eq!(interface["rx_rate_limiter"]["bandwidth"]["size"], 2_500_000);
        assert_eq!(
            interface["tx_rate_limiter"]["bandwidth"]["refill_time"],
            100
        );
    }
}
