//! This device's part in waking the person's devices that sleep
//! ([`tilo_workspace_service::workspaces::person_workspace::wake`]): the daemon keeps the
//! interfaces it can be woken through in the record of the person's devices, sends the magic
//! packets of each wake asked there for a device on one of its LANs, and says it is back from a
//! wake of itself.
//!
//! The interfaces are read from `/sys/class/net` at most every [`ROUTE_SCAN_INTERVAL`], and one
//! that was last seen on a network keeps that subnet while it is off it, or the machine would stop
//! being wakeable the moment it slept. The record is read again whenever it moves.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::PersonResources;
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices};
use tilo_workspace_service::workspaces::person_workspace::wake::{
    self, Ipv4Subnet, MAGIC_PACKET_PORT, WAKE_FACT, WakeRoute,
};
#[cfg(target_os = "linux")]
use tilo_workspace_service::workspaces::person_workspace::wake::{
    MAX_WAKE_ROUTES, WakeInterfaceKind,
};

/// How often the device reads its own interfaces again.
pub const ROUTE_SCAN_INTERVAL: Duration = Duration::from_secs(10);

/// This device's wakes, kept and answered by its daemon.
pub(crate) struct Wake {
    personal: PersonResources,
    cert: DeviceCert,
    /// Its interfaces as last read, the wakes it already took up, and the interfaces it last asked
    /// the record to keep, so a write still waiting for the person's workspace is asked once.
    state: Mutex<Kept>,
}

/// The interfaces as last read, the wakes already taken up, and the interfaces last asked of the
/// record.
type Kept = (Vec<WakeRoute>, BTreeSet<(String, i64)>, String);

impl Wake {
    /// Keep and answer this device's wakes in `personal`, as the device `cert` certifies, on the
    /// daemon's runtime for as long as it runs.
    pub(crate) fn start(personal: PersonResources, cert: DeviceCert) {
        let wake = Arc::new(Self {
            personal,
            cert,
            state: Mutex::default(),
        });
        let mut changes = wake.personal.changes();
        tilo_rt::app().handle().spawn(async move {
            let mut scan = tokio::time::interval(ROUTE_SCAN_INTERVAL);
            loop {
                let rescan = tokio::select! {
                    _ = scan.tick() => true,
                    moved = changes.changed() => {
                        if moved.is_err() {
                            return;
                        }
                        false
                    }
                };
                let wake = Arc::clone(&wake);
                let passed = tokio::task::spawn_blocking(move || {
                    wake.pass(rescan, tilo_clock::unix_now_ns());
                })
                .await;
                if let Err(error) = passed {
                    tracing::warn!(%error, "a pass over this device's wakes stopped");
                }
            }
        });
    }

    /// Read this device's interfaces again when `rescan`, keep them in the record where it says
    /// something else, and answer the wakes the record holds at `now_ns`.
    fn pass(&self, rescan: bool, now_ns: u64) {
        let Some(record) = self.personal.read::<Devices>(DEVICES) else {
            return;
        };
        let own = hex::encode(self.cert.device_public_key);
        let mut state = self.state.lock_recover();
        if rescan || state.0.is_empty() {
            state.0 = wake::remember_subnets(interfaces(), &state.0);
        }
        let routes = state.0.clone();
        let kept = record
            .devices
            .get(&tilo_resource::Id::from_raw(own.clone()))
            .and_then(|device| device.facts.get(WAKE_FACT).cloned());
        let encoded = wake::encoded(&routes);
        if kept.as_deref() != Some(encoded.as_str()) && state.2 != encoded {
            state.2.clone_from(&encoded);
            let certificate = match postcard::to_allocvec(&self.cert) {
                Ok(bytes) => hex::encode(bytes),
                Err(error) => {
                    tracing::warn!(%error, "this device's certificate does not encode");
                    return;
                }
            };
            let keep =
                Devices::KEEP_FACT.with((own.clone(), certificate, WAKE_FACT.to_owned(), encoded));
            self.personal.apply(DEVICES, move || keep.clone());
        }
        let now_ns = i64::try_from(now_ns).unwrap_or(i64::MAX);
        let writes = wake::answering(&record, &own, &routes, now_ns, &mut state.1, send);
        drop(state);
        for write in writes {
            self.personal.apply(DEVICES, move || write.clone());
        }
    }
}

/// Broadcast one magic packet for `mac` on `subnet`. Whether it left.
fn send(mac: [u8; 6], subnet: Ipv4Subnet) -> bool {
    let sent = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)).and_then(|socket| {
        socket.set_broadcast(true)?;
        socket.send_to(
            &wake::magic_packet(mac),
            (subnet.broadcast(), MAGIC_PACKET_PORT),
        )
    });
    sent.inspect_err(|error| {
        tracing::warn!(%error, %subnet, "a wake-up signal could not be sent");
    })
    .is_ok()
}

/// This machine's hardware network interfaces as wake routes.
#[cfg(target_os = "linux")]
fn interfaces() -> Vec<WakeRoute> {
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
        let Some(mac) = wake::parse_mac(read("address").trim()) else {
            continue;
        };
        let kind = if path.join("wireless").exists() || path.join("phy80211").exists() {
            WakeInterfaceKind::WiFi
        } else {
            WakeInterfaceKind::Ethernet
        };
        // Wi-Fi drivers answer the wake query through their radio, not ethtool.
        let magic_packet = match std::fs::read_link(path.join("phy80211")) {
            Ok(radio) => super::capabilities::radio_wakes(
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
fn interfaces() -> Vec<WakeRoute> {
    Vec::new()
}

/// Each interface's first IPv4 subnet.
#[cfg(target_os = "linux")]
#[must_use]
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
