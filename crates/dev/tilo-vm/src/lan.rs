//! The network the VMs tilo-vm runs on one host share.
//!
//! Each VM's own NIC (passt on libvirt) reaches out from the host but not the host's other VMs,
//! so a person's boxes on one host could find each other only through an internet relay: a branch
//! could not be taken in by its source without one, nor two boxes sync or reach each other's agent
//! door. So every libvirt VM gets a second NIC on a network of its host's own: QEMU's socket
//! multicast, kept on the host's loopback (`localaddr=127.0.0.1`), so its frames never leave the
//! host, it needs no root, no bridge and no route, and every VM of the same user on the host is on
//! it. libvirt's own `<interface type='mcast'>` has no local address and would send the frames
//! out of the host's default route, so the NIC is passed to QEMU with `<qemu:commandline>`.
//!
//! The guest gives that NIC an IPv4 address of its own without DHCP: the dev image names a NIC
//! whose MAC starts [`MAC_PREFIX`] `tilolan0` and `tilo-vm-lan.service` gives it
//! `198.18.<5th byte>.<6th byte>/16`, the address [`address`] computes here from the same MAC.
//! A routable IPv4 address rather than a link-local one: the endpoint advertises a link-local
//! address only on a machine with no other (and the passt NIC always gives it one), and the
//! local-network lookup announces itself on the endpoint's IPv4 addresses. 198.18.0.0/15 is set
//! aside for benchmarking (RFC 2544), so it is on no network a host is on, where `10/8`,
//! `172.16/12`, `192.168/16` and Tailscale's `100.64/10` often are: passt gives the guest its
//! host's own address, which this one must not overlap. The NIC has no gateway, so it is never
//! the guest's default route.

use std::net::Ipv4Addr;

use crate::config::VmEntry;

/// The multicast group the frames travel in, on the host's loopback.
pub const GROUP: &str = "239.255.84.76";

/// What every MAC on the network starts with; the dev image names such a NIC `tilolan0`.
pub const MAC_PREFIX: &str = "52:54:01:00";

/// `lan = "off"` in a VM's entry: no second NIC.
pub const OFF: &str = "off";

/// The UDP port of this user's network on this host: one per user (from their home directory,
/// hashed the same way by every tilo-vm), so two people's VMs on one machine are not on one
/// network by accident.
#[must_use]
pub fn port() -> u16 {
    let home = std::env::home_dir().unwrap_or_default();
    // FNV-1a: fixed, unlike std's hasher, so every tilo-vm build picks the same port.
    let hash = home
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
    46_000_u16.saturating_add(u16::try_from(hash % 19_000).unwrap_or(0))
}

/// The MAC of `entry`'s NIC on the network, when it has one.
#[must_use]
pub fn mac(entry: &VmEntry) -> Option<&str> {
    entry.lan.as_deref().filter(|mac| *mac != OFF)
}

/// A MAC for a new NIC on the network that `taken` says no VM has: [`MAC_PREFIX`] and two
/// random bytes, the last neither 0 nor 255, so its [`address`] is an ordinary host address.
#[must_use]
pub fn new_mac(taken: impl Fn(&str) -> bool) -> String {
    loop {
        let [y, z, ..] = *uuid::Uuid::new_v4().as_bytes();
        if z == 0 || z == u8::MAX {
            continue;
        }
        let mac = format!("{MAC_PREFIX}:{y:02x}:{z:02x}");
        if !taken(&mac) {
            return mac;
        }
    }
}

/// The address the guest gives the NIC with MAC `mac`: `198.18.<5th byte>.<6th byte>`.
#[must_use]
pub fn address(mac: &str) -> Option<Ipv4Addr> {
    let rest = mac.to_ascii_lowercase();
    let rest = rest.strip_prefix(MAC_PREFIX)?.strip_prefix(':')?;
    let (y, z) = rest.split_once(':')?;
    Some(Ipv4Addr::new(
        198,
        18,
        u8::from_str_radix(y, 16).ok()?,
        u8::from_str_radix(z, 16).ok()?,
    ))
}

/// Where the NIC sits on the machine's root bus (q35 and aarch64 `virt` both name it `pcie.0`):
/// a slot libvirt never gives its own devices, which it places from slot 1 up (the display at
/// 1, root ports from 2). Without one, QEMU put the NIC in the first free slot, 1, and the
/// display libvirt had placed there failed to start ("slot 1 function 0 not available for
/// virtio-vga, in use by virtio-net-pci", e2e-branch r658951873b).
pub const PCI_PLACE: &str = "bus=pcie.0,addr=0x10";

/// The QEMU arguments of the NIC with MAC `mac`: the socket on the host's loopback, and a virtio
/// NIC on it, at [`PCI_PLACE`].
#[must_use]
pub fn qemu_args(mac: &str) -> [String; 4] {
    [
        "-netdev".into(),
        format!(
            "socket,id=tilolan,mcast={GROUP}:{},localaddr=127.0.0.1",
            port()
        ),
        "-device".into(),
        format!("virtio-net-pci,netdev=tilolan,mac={mac},{PCI_PLACE}"),
    ]
}

/// The libvirt namespace a domain with [`domain_commandline`] declares.
pub const QEMU_NAMESPACE: &str = " xmlns:qemu='http://libvirt.org/schemas/domain/qemu/1.0'";

/// What a libvirt domain carries for the NIC with MAC `mac`: its [`qemu_args`], escaped.
#[must_use]
pub fn domain_commandline(mac: &str) -> String {
    let mut xml = String::from("\n  <qemu:commandline>");
    for arg in qemu_args(mac) {
        xml.push_str("\n    <qemu:arg value='");
        xml.push_str(&quick_xml::escape::escape(arg.as_str()));
        xml.push_str("'/>");
    }
    xml.push_str("\n  </qemu:commandline>");
    xml
}

/// How to tell a person the network: what it is, where it lives, and each VM's address on it.
#[must_use]
pub fn describe() -> String {
    format!(
        "tilo-vm's network on this host (QEMU multicast {GROUP}:{} on 127.0.0.1, guests on 198.18.0.0/16)",
        port()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mac_on_the_network_names_the_address_its_guest_takes() {
        assert_eq!(
            address("52:54:01:00:12:fe"),
            Some(Ipv4Addr::new(198, 18, 18, 254))
        );
        assert_eq!(
            address("52:54:01:00:0A:01"),
            Some(Ipv4Addr::new(198, 18, 10, 1))
        );
        assert_eq!(address("52:54:00:00:12:34"), None, "not a network MAC");
        assert_eq!(address("52:54:01:00:12"), None);
        let mac = new_mac(|_| false);
        let ip = address(&mac).expect("a new MAC has an address");
        assert!(ip.octets()[3] != 0 && ip.octets()[3] != u8::MAX, "{mac}");
        let taken = mac.clone();
        assert_ne!(new_mac(|m| m == taken), mac);
        let entry = VmEntry {
            lan: Some(OFF.into()),
            ..VmEntry::default()
        };
        assert_eq!(super::mac(&entry), None);
        let args = qemu_args("52:54:01:00:01:02");
        assert_eq!(args[0], "-netdev");
        assert!(
            args[1].starts_with("socket,id=tilolan,mcast=239.255.84.76:")
                && args[1].ends_with(",localaddr=127.0.0.1"),
            "{}",
            args[1]
        );
        assert_eq!(
            args[3], "virtio-net-pci,netdev=tilolan,mac=52:54:01:00:01:02,bus=pcie.0,addr=0x10",
            "the NIC names its own slot, which libvirt leaves free"
        );
        assert!((46_000..65_000).contains(&port()));
    }
}
