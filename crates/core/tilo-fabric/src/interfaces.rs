//! This machine's network interfaces as the wake routes it offers the person's other devices: read
//! from `/sys/class/net` and the drivers' magic-packet settings, or fixed in a test that must not
//! depend on the machine running it.

use crate::{Ipv4Subnet, MAX_WAKE_ROUTES, WakeInterfaceKind, WakeRoute};

/// This machine's own network interfaces, as wake routes: read from the system, or fixed in a test
/// that must not depend on the machine running it.
pub trait WakeInterfaces: Send + Sync {
    /// What the interfaces offer now.
    fn routes(&self) -> Vec<WakeRoute>;
}

/// The machine this device runs on: `/sys/class/net` and the drivers' magic-packet settings.
#[derive(Clone, Copy, Debug, Default)]
pub struct SysInterfaces;

impl WakeInterfaces for SysInterfaces {
    fn routes(&self) -> Vec<WakeRoute> {
        scan_interfaces()
    }
}

/// This machine's hardware network interfaces as wake routes.
#[cfg(target_os = "linux")]
fn scan_interfaces() -> Vec<WakeRoute> {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return Vec::new();
    };
    let subnets = ipv4_subnets();
    let mut routes = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // Only hardware: bridges, tunnels, VPNs and loopback wake nothing.
        if !path.join("device").exists() {
            continue;
        }
        let read = |file: &str| std::fs::read_to_string(path.join(file)).unwrap_or_default();
        if read("type").trim() != "1" {
            continue;
        }
        let Some(mac) = crate::wake::parse_mac(read("address").trim()) else {
            continue;
        };
        let kind = if path.join("wireless").exists() || path.join("phy80211").exists() {
            WakeInterfaceKind::WiFi
        } else {
            WakeInterfaceKind::Ethernet
        };
        // Wi-Fi drivers answer the wake query through their radio, not ethtool.
        let magic_packet = match std::fs::read_link(path.join("phy80211")) {
            Ok(radio) => os_actions::network_wake::wifi_magic_packet_enabled(
                &radio.file_name().unwrap_or_default().to_string_lossy(),
            )
            .inspect_err(|reason| {
                tracing::warn!(%reason, device = %name, "a radio's wake setting could not be read");
            })
            .unwrap_or(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                magic_packet_enabled(&name)
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    device = %name,
                    "a device's radio could not be examined; its wake setting is read from the interface"
                );
                magic_packet_enabled(&name)
            }
        };
        let route = WakeRoute {
            mac,
            kind,
            magic_packet,
            subnet: subnets.get(&name).copied(),
        };
        if routes.len() < MAX_WAKE_ROUTES
            && !routes.iter().any(|known: &WakeRoute| known.mac == mac)
        {
            routes.push(route);
        }
    }
    routes.sort_by_key(|route| (route.kind, route.mac));
    routes
}

#[cfg(not(target_os = "linux"))]
fn scan_interfaces() -> Vec<WakeRoute> {
    Vec::new()
}

/// Each interface's first IPv4 subnet.
#[cfg(target_os = "linux")]
pub fn ipv4_subnets() -> std::collections::BTreeMap<String, Ipv4Subnet> {
    let mut subnets = std::collections::BTreeMap::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` fills `head` with a list we free below.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        return subnets;
    }
    let mut cursor = head;
    while !cursor.is_null() {
        // SAFETY: `cursor` walks the list `getifaddrs` returned, which stays
        // valid until `freeifaddrs`.
        let entry = unsafe { &*cursor };
        cursor = entry.ifa_next;
        if entry.ifa_addr.is_null() || entry.ifa_netmask.is_null() || entry.ifa_name.is_null() {
            continue;
        }
        // SAFETY: a non-null `ifa_addr` points at a `sockaddr` whose family
        // says which layout follows.
        if i32::from(unsafe { (*entry.ifa_addr).sa_family }) != libc::AF_INET {
            continue;
        }
        // SAFETY: both are `sockaddr_in` for an `AF_INET` entry.
        let (address, mask) = unsafe {
            (
                (*entry.ifa_addr.cast::<libc::sockaddr_in>())
                    .sin_addr
                    .s_addr,
                (*entry.ifa_netmask.cast::<libc::sockaddr_in>())
                    .sin_addr
                    .s_addr,
            )
        };
        // SAFETY: `ifa_name` is a NUL-terminated interface name.
        let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }
            .to_string_lossy()
            .into_owned();
        let prefix = u32::from_be(mask).count_ones();
        if let Some(subnet) = u8::try_from(prefix).ok().and_then(|prefix| {
            Ipv4Subnet::containing(std::net::Ipv4Addr::from(u32::from_be(address)), prefix)
        }) {
            subnets.entry(name).or_insert(subnet);
        }
    }
    // SAFETY: `head` came from `getifaddrs` and is freed once.
    unsafe { libc::freeifaddrs(head) };
    subnets
}

/// An ioctl request code as the C library types it: glibc's `unsigned long`, musl's `int`, which
/// carries the same 32 bits.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn ioctl_request(code: u32) -> libc::c_ulong {
    libc::c_ulong::from(code)
}

#[cfg(all(target_os = "linux", target_env = "musl"))]
fn ioctl_request(code: u32) -> libc::c_int {
    libc::c_int::from_ne_bytes(code.to_ne_bytes())
}

/// Whether the driver reports magic-packet wake enabled (`ethtool` `g` ).
#[cfg(target_os = "linux")]
fn magic_packet_enabled(interface: &str) -> bool {
    const SIOCETHTOOL: u32 = 0x8946;
    const ETHTOOL_GWOL: u32 = 0x0000_0005;
    const WAKE_MAGIC: u32 = 1 << 5;
    #[repr(C)]
    struct WolInfo {
        cmd: u32,
        supported: u32,
        wolopts: u32,
        sopass: [u8; 6],
    }
    #[repr(C)]
    struct Request {
        name: [libc::c_char; libc::IFNAMSIZ],
        data: *mut libc::c_void,
        _union_rest: [u8; 16],
    }
    let bytes = interface.as_bytes();
    if bytes.len() >= libc::IFNAMSIZ {
        return false;
    }
    let mut wol = WolInfo {
        cmd: ETHTOOL_GWOL,
        supported: 0,
        wolopts: 0,
        sopass: [0; 6],
    };
    let mut request = Request {
        name: [0; libc::IFNAMSIZ],
        data: (&raw mut wol).cast(),
        _union_rest: [0; 16],
    };
    for (slot, byte) in request.name.iter_mut().zip(bytes) {
        *slot = libc::c_char::from_ne_bytes([*byte]);
    }
    // SAFETY: a datagram socket only to carry the ioctl; closed below.
    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if socket < 0 {
        return false;
    }
    // SAFETY: `request` is an `ifreq` whose data points at a live
    // `ethtool_wolinfo` for the duration of the call.
    let answered =
        unsafe { libc::ioctl(socket, ioctl_request(SIOCETHTOOL), &raw mut request) } == 0;
    // SAFETY: `socket` is ours and closed once.
    unsafe { libc::close(socket) };
    answered && wol.wolopts & WAKE_MAGIC != 0
}
