//! Waking one of the person's sleeping devices.
//!
//! A device advertises its [`WakeRoute`]s in its certified presence: the MAC
//! each interface shows the network, whether a magic packet wakes it, and the
//! IPv4 subnet it was last seen on. `device.wake` picks a device that is
//! awake on the target's last subnet, this one first. That device sends the
//! magic packet (UDP broadcast, port 9) and probes the target until it
//! answers, then reports back. Without an awake device on any of the
//! target's subnets the request fails at once and names them.
//!
//! The waker reads the target's routes from its own copy of the person's
//! fabric, so a request can never make a device send packets for a MAC
//! that is not one of the person's own devices.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::shell::device_fabric::{
    DeviceFabric, Ipv4Subnet, PersonalFabricAction, PersonalFabricActionEnvelope,
    WakeInterfaceKind, WakeOutcome, WakeRoute,
};
use crate::shell::network::ShellNetwork;
use crate::shell::personal_continuity::DeviceId;

/// The discard port Wake-on-LAN senders conventionally use.
pub const MAGIC_PACKET_PORT: u16 = 9;
/// How long a waker waits for the target to answer before giving up.
pub const WAKE_REJOIN_TIMEOUT: Duration = Duration::from_secs(150);
/// How often the waker asks the target whether it is back.
pub const WAKE_PROBE_INTERVAL: Duration = Duration::from_secs(3);
/// How long a requester waits for a remote waker's final report.
const WAKE_REPORT_TIMEOUT: Duration = Duration::from_secs(180);
/// How often this device re-reads its own interfaces.
const ROUTE_SCAN_INTERVAL: Duration = Duration::from_secs(10);
const MAX_PENDING_WAKES: usize = 16;
const MAX_WAKE_EVENTS: usize = 32;

/// Six `0xFF` bytes, then the target MAC sixteen times.
#[must_use]
pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut packet = [0xFF; 102];
    for chunk in packet.chunks_exact_mut(6).skip(1) {
        chunk.copy_from_slice(&mac);
    }
    packet
}

/// Why a device cannot be woken right now.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WakeRefusal {
    #[error("no device of yours is called {0}")]
    UnknownDevice(String),
    #[error("{0} names more than one of your devices; use its device id")]
    AmbiguousDevice(String),
    #[error("this device is already awake")]
    LocalDevice,
    #[error("{0} can't be woken over the network")]
    NoWakeRoute(String),
    #[error("no awake device on {}", join_subnets(.0))]
    NoAwakeSibling(Vec<Ipv4Subnet>),
}

fn join_subnets(subnets: &[Ipv4Subnet]) -> String {
    subnets
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" or ")
}

/// Which device sends the magic packet, and on which subnet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakePlan {
    pub target: DeviceId,
    pub waker: DeviceId,
    pub subnet: Ipv4Subnet,
}

/// The device `name` names among the person's devices: its id, or else the
/// one device whose label matches it ignoring case.
///
/// # Errors
///
/// Returns why `name` names no single device.
pub fn resolve_device(fabric: &DeviceFabric, name: &str) -> Result<DeviceId, WakeRefusal> {
    let name = name.trim();
    if let Some(snapshot) = fabric
        .snapshots()
        .values()
        .find(|snapshot| snapshot.owner.as_str() == name)
    {
        return Ok(snapshot.owner.clone());
    }
    let mut matches = fabric
        .snapshots()
        .values()
        .filter(|snapshot| snapshot.presence.label.eq_ignore_ascii_case(name))
        .map(|snapshot| snapshot.owner.clone());
    match (matches.next(), matches.next()) {
        (Some(device), None) => Ok(device),
        (Some(_), Some(_)) => Err(WakeRefusal::AmbiguousDevice(name.to_owned())),
        (None, _) => Err(WakeRefusal::UnknownDevice(name.to_owned())),
    }
}

/// The subnets a magic packet can wake `target` on, in its own order.
fn wakeable_subnets(fabric: &DeviceFabric, target: &DeviceId) -> Vec<Ipv4Subnet> {
    let mut subnets = Vec::new();
    for route in fabric
        .snapshots()
        .get(target)
        .map(|snapshot| snapshot.presence.wake_routes.as_slice())
        .unwrap_or_default()
    {
        if let (true, Some(subnet)) = (wakes(route), route.subnet)
            && !subnets.contains(&subnet)
        {
            subnets.push(subnet);
        }
    }
    subnets
}

/// Whether a magic packet wakes the device through `route`: only what the
/// device reports enabled. Nothing assumes a kind wakes; a Wi-Fi chip may
/// list `WoWLAN` while the machine around it does not wake from it, so the
/// image enables it only where that is verified.
fn wakes(route: &WakeRoute) -> bool {
    route.magic_packet
}

fn on_subnet(routes: &[WakeRoute], subnet: Ipv4Subnet) -> bool {
    routes.iter().any(|route| route.subnet == Some(subnet))
}

/// Pick the device that wakes `target`: this device when it is on one of
/// the target's subnets, else the first awake sibling on one, in subnet
/// order and then device order.
///
/// # Errors
///
/// Returns why nothing can wake the target, naming the subnets searched.
pub fn plan_wake(
    fabric: &DeviceFabric,
    local: &DeviceId,
    local_routes: &[WakeRoute],
    target: &DeviceId,
    awake: impl Fn(&DeviceId) -> bool,
) -> Result<WakePlan, WakeRefusal> {
    if target == local {
        return Err(WakeRefusal::LocalDevice);
    }
    let label = fabric
        .snapshots()
        .get(target)
        .map(|snapshot| snapshot.presence.label.clone())
        .ok_or_else(|| WakeRefusal::UnknownDevice(target.as_str().to_owned()))?;
    let subnets = wakeable_subnets(fabric, target);
    if subnets.is_empty() {
        return Err(WakeRefusal::NoWakeRoute(label));
    }
    let plan = |waker: &DeviceId, subnet: Ipv4Subnet| WakePlan {
        target: target.clone(),
        waker: waker.clone(),
        subnet,
    };
    if let Some(subnet) = subnets
        .iter()
        .copied()
        .find(|subnet| on_subnet(local_routes, *subnet))
    {
        return Ok(plan(local, subnet));
    }
    for subnet in &subnets {
        if let Some(sibling) = fabric.snapshots().values().find(|snapshot| {
            &snapshot.owner != target
                && &snapshot.owner != local
                && awake(&snapshot.owner)
                && on_subnet(&snapshot.presence.wake_routes, *subnet)
        }) {
            return Ok(plan(&sibling.owner, *subnet));
        }
    }
    Err(WakeRefusal::NoAwakeSibling(subnets))
}

/// The packets this device sends to wake `target`: one per wakeable
/// interface of the target on a subnet this device is on.
#[must_use]
pub fn magic_packets_for(
    fabric: &DeviceFabric,
    local_routes: &[WakeRoute],
    target: &DeviceId,
) -> Vec<([u8; 6], Ipv4Subnet)> {
    fabric
        .snapshots()
        .get(target)
        .map(|snapshot| snapshot.presence.wake_routes.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|route| wakes(route))
        .filter_map(|route| Some((route.mac, route.subnet?)))
        .filter(|(_, subnet)| on_subnet(local_routes, *subnet))
        .collect()
}

/// Keep the subnet an interface was last seen on while it has none now, so
/// a device that went to sleep off its network stays wakeable once back.
#[must_use]
pub fn remember_subnets(mut current: Vec<WakeRoute>, previous: &[WakeRoute]) -> Vec<WakeRoute> {
    for route in &mut current {
        if route.subnet.is_none() {
            route.subnet = previous
                .iter()
                .find(|old| old.mac == route.mac)
                .and_then(|old| old.subnet);
        }
    }
    current
}

/// One device the waker is watching for after sending it a magic packet.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Watch {
    /// Who asked: `None` when it was this device.
    requester: Option<DeviceId>,
    started: Instant,
    last_probe: Option<Instant>,
    nonce: [u8; 16],
}

/// Something the waker must do next for one watched device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchStep {
    Probe {
        target: DeviceId,
        nonce: [u8; 16],
    },
    Finished {
        target: DeviceId,
        requester: Option<DeviceId>,
        outcome: WakeOutcome,
    },
}

/// The devices this one woke and is waiting to hear back from.
#[derive(Clone, Debug, Default)]
pub struct WakeWatch {
    watching: BTreeMap<DeviceId, Watch>,
}

impl WakeWatch {
    /// Start watching `target`, replacing an older watch for it.
    pub fn start(
        &mut self,
        target: DeviceId,
        requester: Option<DeviceId>,
        nonce: [u8; 16],
        now: Instant,
    ) -> bool {
        if !self.watching.contains_key(&target) && self.watching.len() >= MAX_PENDING_WAKES {
            return false;
        }
        self.watching.insert(
            target,
            Watch {
                requester,
                started: now,
                last_probe: None,
                nonce,
            },
        );
        true
    }

    /// Probes that are due and watches that ran out of time.
    pub fn poll(&mut self, now: Instant) -> Vec<WatchStep> {
        let mut steps = Vec::new();
        self.watching.retain(|target, watch| {
            if now.saturating_duration_since(watch.started) >= WAKE_REJOIN_TIMEOUT {
                steps.push(WatchStep::Finished {
                    target: target.clone(),
                    requester: watch.requester.clone(),
                    outcome: WakeOutcome::NotRejoined,
                });
                return false;
            }
            if watch
                .last_probe
                .is_none_or(|last| now.saturating_duration_since(last) >= WAKE_PROBE_INTERVAL)
            {
                watch.last_probe = Some(now);
                steps.push(WatchStep::Probe {
                    target: target.clone(),
                    nonce: watch.nonce,
                });
            }
            true
        });
        steps
    }

    /// `from` answered a probe: when it is the watched device answering its
    /// own probe, it is awake again.
    pub fn answered(&mut self, from: &DeviceId, nonce: [u8; 16]) -> Option<WatchStep> {
        if self.watching.get(from)?.nonce != nonce {
            return None;
        }
        let watch = self.watching.remove(from)?;
        Some(WatchStep::Finished {
            target: from.clone(),
            requester: watch.requester,
            outcome: WakeOutcome::Rejoined,
        })
    }
}

/// What the person who asked to wake a device is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeEvent {
    pub target: DeviceId,
    pub label: String,
    pub outcome: WakeOutcome,
}

#[derive(Default)]
struct WakeState {
    watch: WakeWatch,
    /// Wakes this device asked another one to perform: target → (waker, asked at).
    requested: BTreeMap<DeviceId, (DeviceId, Instant)>,
    events: VecDeque<WakeEvent>,
    routes: Vec<WakeRoute>,
    scanned: Option<Instant>,
}

fn wake_state() -> &'static Mutex<WakeState> {
    static STATE: OnceLock<Mutex<WakeState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(WakeState::default()))
}

fn label_of(fabric: &DeviceFabric, device: &DeviceId) -> String {
    fabric.snapshots().get(device).map_or_else(
        || device.as_str().to_owned(),
        |snapshot| snapshot.presence.label.clone(),
    )
}

fn push_event(
    state: &mut WakeState,
    fabric: &DeviceFabric,
    target: &DeviceId,
    outcome: WakeOutcome,
) {
    if state.events.len() >= MAX_WAKE_EVENTS {
        state.events.pop_front();
    }
    let label = label_of(fabric, target);
    tracing::info!(
        target: "tilo::device_wake",
        device = label.as_str(),
        ?outcome,
        "wake request progressed"
    );
    state.events.push_back(WakeEvent {
        target: target.clone(),
        label,
        outcome,
    });
}

/// This device's own wake routes, re-read from its interfaces at most every
/// `ROUTE_SCAN_INTERVAL`. `previous` is what it last advertised.
pub fn local_routes(previous: &[WakeRoute]) -> Vec<WakeRoute> {
    let now = Instant::now();
    {
        let Ok(state) = wake_state().lock() else {
            return previous.to_vec();
        };
        if state
            .scanned
            .is_some_and(|at| now.saturating_duration_since(at) < ROUTE_SCAN_INTERVAL)
        {
            return state.routes.clone();
        }
    }
    // The scan's ethtool ioctls wait on the kernel's rtnl lock. The wake
    // state stays free meanwhile: the compositor takes it for a wake request.
    let routes = remember_subnets(scan_interfaces(), previous);
    let Ok(mut state) = wake_state().lock() else {
        return routes;
    };
    state.routes = routes;
    state.scanned = Some(now);
    state.routes.clone()
}

fn awake(network: &ShellNetwork, local: &DeviceId, device: &DeviceId) -> bool {
    device == local || network.current_personal_route(device).is_some()
}

/// Wake the device `name` names (its id or its label).
///
/// # Errors
///
/// Returns why it cannot be woken, e.g. `no awake device on 10.0.0.0/24`.
pub fn request_wake(network: &ShellNetwork, name: &str) -> Result<WakePlan, String> {
    let (local, _) = network
        .local_device_fabric_identity()
        .ok_or_else(|| "this device has no certified identity".to_string())?;
    let fabric = network.device_fabric();
    let target = resolve_device(&fabric, name).map_err(|refusal| refusal.to_string())?;
    let local_routes = fabric
        .snapshots()
        .get(&local)
        .map(|snapshot| snapshot.presence.wake_routes.clone())
        .unwrap_or_default();
    let plan = plan_wake(&fabric, &local, &local_routes, &target, |device| {
        awake(network, &local, device)
    })
    .map_err(|refusal| refusal.to_string())?;
    if plan.waker == local {
        let outcome = wake_from_here(network, &fabric, &target, None);
        if let Ok(mut state) = wake_state().lock() {
            push_event(&mut state, &fabric, &target, outcome);
        }
        return match outcome {
            WakeOutcome::Sent { .. } => Ok(plan),
            _ => Err(format!(
                "could not send a wake-up signal to {} on {}",
                label_of(&fabric, &target),
                plan.subnet
            )),
        };
    }
    network
        .send_personal_fabric_action(
            &plan.waker,
            PersonalFabricAction::WakeDevice {
                target: target.clone(),
            },
        )
        .map_err(|error| {
            format!(
                "{} could not be asked to wake {}: {error}",
                label_of(&fabric, &plan.waker),
                label_of(&fabric, &target)
            )
        })?;
    if let Ok(mut state) = wake_state().lock() {
        state
            .requested
            .insert(target, (plan.waker.clone(), Instant::now()));
    }
    Ok(plan)
}

/// Send the magic packets for `target` from this device and start watching
/// for it on behalf of `requester`.
fn wake_from_here(
    network: &ShellNetwork,
    fabric: &DeviceFabric,
    target: &DeviceId,
    requester: Option<DeviceId>,
) -> WakeOutcome {
    let previous = network
        .local_device_fabric_identity()
        .and_then(|(local, _)| fabric.snapshots().get(&local).cloned())
        .map(|snapshot| snapshot.presence.wake_routes)
        .unwrap_or_default();
    let routes = local_routes(&previous);
    let packets = magic_packets_for(fabric, &routes, target);
    let Some((_, subnet)) = packets.first().copied() else {
        return WakeOutcome::NotOnTargetLan;
    };
    let mut sent = false;
    for (mac, subnet) in &packets {
        match send_magic_packet(*mac, *subnet) {
            Ok(()) => sent = true,
            Err(error) => tracing::warn!(
                target: "tilo::device_wake",
                %error,
                %subnet,
                "magic packet could not be sent"
            ),
        }
    }
    if !sent {
        return WakeOutcome::SendFailed;
    }
    if let Ok(mut state) = wake_state().lock() {
        state.watch.start(
            target.clone(),
            requester,
            crate::random::nonzero::<16>(),
            Instant::now(),
        );
    }
    WakeOutcome::Sent { subnet }
}

/// Broadcast one magic packet for `mac` on `subnet`.
///
/// # Errors
///
/// Returns the socket error.
pub fn send_magic_packet(mac: [u8; 6], subnet: Ipv4Subnet) -> std::io::Result<()> {
    let socket = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.send_to(&magic_packet(mac), (subnet.broadcast(), MAGIC_PACKET_PORT))?;
    Ok(())
}

/// Handle a wake action from another of the person's devices. Returns
/// whether it was one.
pub(crate) fn handle_action(
    network: &ShellNetwork,
    envelope: &PersonalFabricActionEnvelope,
) -> bool {
    match &envelope.action {
        PersonalFabricAction::WakeDevice { target } => {
            let fabric = network.device_fabric();
            let outcome = wake_from_here(network, &fabric, target, Some(envelope.source.clone()));
            let _ = network.send_personal_fabric_action(
                &envelope.source,
                PersonalFabricAction::WakeReport {
                    target: target.clone(),
                    outcome,
                },
            );
        }
        PersonalFabricAction::WakeReport { target, outcome } => {
            let fabric = network.device_fabric();
            if let Ok(mut state) = wake_state().lock()
                && state
                    .requested
                    .get(target)
                    .is_some_and(|(waker, _)| waker == &envelope.source)
            {
                if !matches!(outcome, WakeOutcome::Sent { .. }) {
                    state.requested.remove(target);
                }
                push_event(&mut state, &fabric, target, *outcome);
            }
        }
        PersonalFabricAction::WakeProbe { nonce } => {
            let _ = network.send_personal_fabric_action(
                &envelope.source,
                PersonalFabricAction::WakeProbeReply { nonce: *nonce },
            );
        }
        PersonalFabricAction::WakeProbeReply { nonce } => {
            let step = wake_state()
                .lock()
                .ok()
                .and_then(|mut state| state.watch.answered(&envelope.source, *nonce));
            if let Some(step) = step {
                finish(network, step);
            }
        }
        _ => return false,
    }
    true
}

fn finish(network: &ShellNetwork, step: WatchStep) {
    let WatchStep::Finished {
        target,
        requester,
        outcome,
    } = step
    else {
        return;
    };
    if let Some(requester) = requester {
        let _ = network.send_personal_fabric_action(
            &requester,
            PersonalFabricAction::WakeReport { target, outcome },
        );
    } else {
        let fabric = network.device_fabric();
        if let Ok(mut state) = wake_state().lock() {
            push_event(&mut state, &fabric, &target, outcome);
        }
    }
}

/// Probe the devices this one is waking and give up on the ones that do not
/// come back; forget requests whose waker never reported.
pub(crate) fn pump(network: &ShellNetwork) {
    let now = Instant::now();
    let (steps, abandoned) = {
        let Ok(mut state) = wake_state().lock() else {
            return;
        };
        let steps = state.watch.poll(now);
        let mut abandoned = Vec::new();
        state.requested.retain(|target, (_, asked)| {
            let waiting = now.saturating_duration_since(*asked) < WAKE_REPORT_TIMEOUT;
            if !waiting {
                abandoned.push(target.clone());
            }
            waiting
        });
        (steps, abandoned)
    };
    if !abandoned.is_empty() {
        let fabric = network.device_fabric();
        if let Ok(mut state) = wake_state().lock() {
            for target in abandoned {
                push_event(&mut state, &fabric, &target, WakeOutcome::NotRejoined);
            }
        }
    }
    for step in steps {
        match step {
            WatchStep::Probe { target, nonce } => {
                // Unanswered until the target's route is back.
                let _ = network.send_personal_fabric_action(
                    &target,
                    PersonalFabricAction::WakeProbe { nonce },
                );
            }
            finished @ WatchStep::Finished { .. } => finish(network, finished),
        }
    }
}

/// Outcomes of this device's wake requests since the last call.
pub(crate) fn drain_events() -> Vec<WakeEvent> {
    wake_state()
        .lock()
        .map(|mut state| state.events.drain(..).collect())
        .unwrap_or_default()
}

/// This device's hardware network interfaces as wake routes.
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
        let Some(mac) = parse_mac(read("address").trim()) else {
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
            .unwrap_or(false),
            Err(_) => magic_packet_enabled(&name),
        };
        let route = WakeRoute {
            mac,
            kind,
            magic_packet,
            subnet: subnets.get(&name).copied(),
        };
        if routes.len() < crate::shell::device_fabric::MAX_WAKE_ROUTES
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

/// `aa:bb:cc:dd:ee:ff` as bytes, refusing anything that cannot address a NIC.
#[must_use]
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut mac = [0; 6];
    let mut parts = text.split(':');
    for octet in &mut mac {
        let part = parts.next()?;
        if part.len() != 2 {
            return None;
        }
        *octet = u8::from_str_radix(part, 16).ok()?;
    }
    (parts.next().is_none() && mac != [0; 6] && mac.first().is_some_and(|first| first & 1 == 0))
        .then_some(mac)
}

/// Each interface's first IPv4 subnet.
#[cfg(target_os = "linux")]
pub(crate) fn ipv4_subnets() -> BTreeMap<String, Ipv4Subnet> {
    let mut subnets = BTreeMap::new();
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

/// Whether the driver reports magic-packet wake enabled (`ethtool` `g`).
#[cfg(target_os = "linux")]
fn magic_packet_enabled(interface: &str) -> bool {
    const SIOCETHTOOL: libc::c_ulong = 0x8946;
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
    let answered = unsafe { libc::ioctl(socket, SIOCETHTOOL, &raw mut request) } == 0;
    // SAFETY: `socket` is ours and closed once.
    unsafe { libc::close(socket) };
    answered && wol.wolopts & WAKE_MAGIC != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::device_fabric::{
        DeviceFabricSnapshot, DeviceKind, DevicePresence, FABRIC_SCHEMA_VERSION,
        FabricCapabilities, FabricStamp, OwnerSession,
    };

    fn id(value: &str) -> DeviceId {
        DeviceId::new(value).unwrap()
    }

    fn subnet(text: &str) -> Ipv4Subnet {
        let (address, prefix) = text.split_once('/').unwrap();
        Ipv4Subnet::containing(address.parse().unwrap(), prefix.parse().unwrap()).unwrap()
    }

    fn route(mac: u8, magic_packet: bool, on: Option<&str>) -> WakeRoute {
        WakeRoute {
            mac: [0x52, 0x54, 0, 0, 0, mac],
            kind: WakeInterfaceKind::Ethernet,
            magic_packet,
            subnet: on.map(subnet),
        }
    }

    fn device(name: &str, label: &str, routes: Vec<WakeRoute>) -> DeviceFabricSnapshot {
        let owner = id(name);
        DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: owner.clone(),
            presence: DevicePresence {
                device: owner.clone(),
                label: label.into(),
                kind: DeviceKind::Laptop,
                capabilities: FabricCapabilities::default(),
                online: true,
                last_seen_unix_ms: 1,
                wake_routes: routes,
                session: OwnerSession::default(),
                stamp: FabricStamp {
                    counter: 1,
                    device: owner,
                },
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: Vec::new(),
            wifi: Vec::new(),
            display: None,
            unlock: crate::shell::device_unlock::UnlockFabricState::default(),
            speakers: crate::shell::speaker_group::SpeakerFabricState::default(),
        }
    }

    fn fabric(devices: Vec<DeviceFabricSnapshot>) -> DeviceFabric {
        let mut fabric = DeviceFabric::default();
        for snapshot in devices {
            fabric.upsert(snapshot).unwrap();
        }
        fabric
    }

    #[test]
    fn a_magic_packet_is_six_ff_then_sixteen_copies_of_the_mac() {
        let mac = [0x52, 0x54, 0x00, 0x23, 0x83, 0x0b];
        let packet = magic_packet(mac);
        assert_eq!(packet.len(), 102);
        assert_eq!(packet.get(..6), Some(&[0xFF; 6][..]));
        assert!(packet.get(6..).unwrap().chunks(6).all(|chunk| chunk == mac));
        assert_eq!(
            subnet("192.168.231.11/24").broadcast().to_string(),
            "192.168.231.255"
        );
        assert_eq!(subnet("10.1.2.3/16").to_string(), "10.1.0.0/16");
        assert_eq!(
            Ipv4Subnet::containing("10.0.0.1".parse().unwrap(), 32),
            None
        );
        assert_eq!(parse_mac("52:54:00:23:83:0b"), Some(mac));
        assert_eq!(parse_mac("01:00:5e:00:00:01"), None, "multicast");
        assert_eq!(parse_mac("00:00:00:00:00:00"), None);
        assert_eq!(parse_mac("52:54:00:23:83"), None);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario walks every waker choice"
    )]
    fn the_waker_is_an_awake_device_on_the_targets_last_subnet() {
        let lan = "192.168.1.0/24";
        let office = "10.0.0.0/24";
        let target = device(
            "desk",
            "Desk",
            vec![
                route(1, true, Some(lan)),
                route(2, false, Some("172.16.0.0/24")),
            ],
        );
        let here = device("laptop", "Laptop", vec![route(3, false, Some(lan))]);
        let sibling = device("server", "Server", vec![route(4, false, Some(lan))]);
        let elsewhere = device("phone", "Phone", vec![route(5, false, Some(office))]);
        let all = fabric(vec![
            target.clone(),
            here.clone(),
            sibling,
            elsewhere.clone(),
        ]);

        // This device is on the LAN: it wakes the target itself.
        let local_routes = here.presence.wake_routes.clone();
        assert_eq!(
            plan_wake(&all, &id("laptop"), &local_routes, &id("desk"), |_| true),
            Ok(WakePlan {
                target: id("desk"),
                waker: id("laptop"),
                subnet: subnet(lan),
            })
        );
        assert_eq!(
            magic_packets_for(&all, &local_routes, &id("desk")),
            vec![([0x52, 0x54, 0, 0, 0, 1], subnet(lan))],
            "only the interface that wakes on a magic packet is sent to"
        );

        // Away from home, an awake sibling on the LAN does it.
        let away = elsewhere.presence.wake_routes.clone();
        assert_eq!(
            plan_wake(&all, &id("phone"), &away, &id("desk"), |device| device
                == &id("server")),
            Ok(WakePlan {
                target: id("desk"),
                waker: id("server"),
                subnet: subnet(lan),
            })
        );
        assert!(magic_packets_for(&all, &away, &id("desk")).is_empty());

        // Nobody awake there: the refusal names the subnet.
        let refusal = plan_wake(&all, &id("phone"), &away, &id("desk"), |_| false).unwrap_err();
        assert_eq!(refusal.to_string(), "no awake device on 192.168.1.0/24");

        // The target itself never counts as its own waker.
        let only_target = fabric(vec![target, elsewhere]);
        assert_eq!(
            plan_wake(&only_target, &id("phone"), &away, &id("desk"), |_| true),
            Err(WakeRefusal::NoAwakeSibling(vec![subnet(lan)]))
        );

        // No wakeable interface, or this device, or nobody by that name.
        let deaf = fabric(vec![
            device("tv", "TV", vec![route(6, false, Some(lan))]),
            here,
        ]);
        assert_eq!(
            plan_wake(&deaf, &id("laptop"), &local_routes, &id("tv"), |_| true),
            Err(WakeRefusal::NoWakeRoute("TV".into()))
        );
        let mut wifi = route(7, false, Some(lan));
        wifi.kind = WakeInterfaceKind::WiFi;
        let radio = fabric(vec![device("tablet", "Tablet", vec![wifi])]);
        assert_eq!(
            plan_wake(&radio, &id("laptop"), &local_routes, &id("tablet"), |_| {
                true
            })
            .unwrap_err()
            .to_string(),
            "Tablet can't be woken over the network",
            "an interface without magic-packet wake enabled is no wake route"
        );
        wifi.magic_packet = true;
        let woken = fabric(vec![device("tablet", "Tablet", vec![wifi])]);
        assert_eq!(
            plan_wake(&woken, &id("laptop"), &local_routes, &id("tablet"), |_| {
                true
            })
            .map(|plan| plan.waker),
            Ok(id("laptop")),
            "Wi-Fi with WoWLAN enabled wakes like Ethernet"
        );
        assert_eq!(
            plan_wake(&deaf, &id("laptop"), &local_routes, &id("laptop"), |_| true),
            Err(WakeRefusal::LocalDevice)
        );
        assert_eq!(
            resolve_device(&all, "desk"),
            Ok(id("desk")),
            "a device id resolves as itself"
        );
        assert_eq!(resolve_device(&all, " server "), Ok(id("server")));
        assert_eq!(
            resolve_device(&all, "SERVER"),
            Ok(id("server")),
            "a label matches ignoring case"
        );
        assert_eq!(
            resolve_device(&all, "Toaster"),
            Err(WakeRefusal::UnknownDevice("Toaster".into()))
        );
        let twins = fabric(vec![
            device("a", "Pi", Vec::new()),
            device("b", "pi", Vec::new()),
        ]);
        assert_eq!(
            resolve_device(&twins, "pi"),
            Err(WakeRefusal::AmbiguousDevice("pi".into()))
        );
    }

    #[test]
    fn a_route_keeps_its_last_subnet_while_off_the_network() {
        let previous = vec![route(1, true, Some("192.168.1.0/24"))];
        let now = remember_subnets(vec![route(1, true, None), route(2, true, None)], &previous);
        assert_eq!(
            now.first().and_then(|route| route.subnet),
            Some(subnet("192.168.1.0/24"))
        );
        assert_eq!(now.get(1).and_then(|route| route.subnet), None);
    }

    #[test]
    fn the_waker_reports_a_rejoin_or_gives_up() {
        let start = Instant::now();
        let mut watch = WakeWatch::default();
        assert!(watch.start(id("desk"), Some(id("phone")), [7; 16], start));
        assert_eq!(
            watch.poll(start),
            vec![WatchStep::Probe {
                target: id("desk"),
                nonce: [7; 16]
            }]
        );
        assert!(
            watch.poll(start + Duration::from_secs(1)).is_empty(),
            "probes are paced"
        );
        assert_eq!(watch.answered(&id("desk"), [8; 16]), None, "a stale nonce");
        assert_eq!(
            watch.answered(&id("phone"), [7; 16]),
            None,
            "another device"
        );
        assert_eq!(
            watch.answered(&id("desk"), [7; 16]),
            Some(WatchStep::Finished {
                target: id("desk"),
                requester: Some(id("phone")),
                outcome: WakeOutcome::Rejoined,
            })
        );
        assert!(watch.poll(start + WAKE_PROBE_INTERVAL).is_empty());

        assert!(watch.start(id("desk"), None, [9; 16], start));
        let _ = watch.poll(start);
        assert_eq!(
            watch.poll(start + WAKE_REJOIN_TIMEOUT),
            vec![WatchStep::Finished {
                target: id("desk"),
                requester: None,
                outcome: WakeOutcome::NotRejoined,
            }]
        );
    }
}
