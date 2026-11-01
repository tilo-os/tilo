//! Waking one of the person's sleeping devices from this one.
//!
//! A device that went to sleep off the network still answers a magic packet,
//! and whichever of the person's devices shares its LAN sends it: this one
//! when it does, one of its siblings otherwise ([`tilo_device_fabric::wake`]).
//! This mount is the session's side of that — which interfaces this machine
//! offers to be woken on, the packets it sends, the probes it answers, and
//! what the person is told once the device comes back or does not.
//!
//! The routes this machine offers travel in its own record, so its devices
//! know whether a packet from here could reach a sleeping one: they are read
//! from `/sys/class/net` at most every [`ROUTE_SCAN_INTERVAL`], and an
//! interface that was last seen on a network keeps that subnet while it is
//! off it, or the machine would stop being wakeable the moment it slept.
//!
//! Everything the fabric decides is in the crate's own module; here is only
//! what touches the machine: the socket, the interfaces, the clock.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use tilo_device_fabric::link::SharedLink;
use tilo_device_fabric::wake::{
    self, MagicPackets, ROUTE_SCAN_INTERVAL, UdpMagicPackets, WakeEvent, WakePlan, Waker, WatchStep,
};
use tilo_device_fabric::{
    DeviceId, Ipv4Subnet, MAX_WAKE_ROUTES, PersonalFabricAction, PersonalFabricActionEnvelope,
    WakeInterfaceKind, WakeOutcome, WakeRoute,
};

use crate::{Shared, notifications::SessionNotifications};

/// This machine's own network interfaces, as wake routes: read from the
/// system, or fixed in a test that must not depend on the machine running
/// it.
pub trait WakeInterfaces: Send + Sync {
    /// What the interfaces offer now.
    fn routes(&self) -> Vec<WakeRoute>;
}

/// The machine this session runs on: `/sys/class/net` and the drivers'
/// magic-packet settings.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SysInterfaces;

impl WakeInterfaces for SysInterfaces {
    fn routes(&self) -> Vec<WakeRoute> {
        scan_interfaces()
    }
}

/// Where a session's wakes send their packets and what its interfaces offer,
/// so a test keeps both off the machine it runs on.
#[derive(Clone, Default)]
pub struct Seams {
    pub packets: Option<Arc<dyn MagicPackets>>,
    pub interfaces: Option<Arc<dyn WakeInterfaces>>,
}

/// This session's wakes: what it sends, what it answers and what it waits
/// for. Called once per pass of the session's loop.
pub(crate) struct Wake {
    /// The socket the packets leave through; a test's own, so no test
    /// broadcasts.
    packets: Arc<dyn MagicPackets>,
    interfaces: Arc<dyn WakeInterfaces>,
    waker: RefCell<Waker>,
    /// This machine's own wake routes, as its interfaces last showed them.
    routes: RefCell<Vec<WakeRoute>>,
    /// When the interfaces were last read.
    scanned: RefCell<Option<Instant>>,
}

impl Wake {
    /// The session's wakes over `seams`: the real socket and this machine's
    /// interfaces unless a test brought its own.
    pub(crate) fn new(seams: &Seams) -> Rc<Self> {
        Rc::new(Self {
            packets: seams
                .packets
                .clone()
                .unwrap_or_else(|| Arc::new(UdpMagicPackets)),
            interfaces: seams
                .interfaces
                .clone()
                .unwrap_or_else(|| Arc::new(SysInterfaces)),
            waker: RefCell::new(Waker::default()),
            routes: RefCell::new(Vec::new()),
            scanned: RefCell::new(None),
        })
    }

    /// Offer this machine's interfaces to the person's devices, probe the
    /// devices it woke, give up on the ones that do not come back, and tell
    /// the person how each of their requests ended. Called every pass.
    pub(crate) fn follow(&self, shared: &Shared) {
        let Some(fabric) = &shared.fabric else {
            return;
        };
        self.publish(fabric);
        // A device back on the person's fabric is awake: its session coming
        // up is the answer a probe would have asked for.
        let online = fabric.online_device_ids();
        let back = self.waker.borrow_mut().rejoins(&online);
        for step in back {
            self.carry_out(shared, fabric, step);
        }
        let now = Instant::now();
        let steps = self.waker.borrow_mut().poll(now);
        for step in steps {
            self.carry_out(shared, fabric, step);
        }
        self.waker
            .borrow_mut()
            .abandoned(now, |device| label_of(fabric, device));
        self.tell(shared);
    }

    /// Answer the wake actions one of the person's other devices sent this
    /// one. Whether `envelope` was one of them.
    pub(crate) fn handle_action(
        &self,
        shared: &Shared,
        fabric: &SharedLink,
        envelope: &PersonalFabricActionEnvelope,
    ) -> bool {
        match &envelope.action {
            PersonalFabricAction::WakeDevice { target } => {
                let outcome = self.wake_from_here(fabric, target, Some(envelope.source.clone()));
                answer(
                    fabric,
                    &envelope.source,
                    PersonalFabricAction::WakeReport {
                        target: target.clone(),
                        outcome,
                    },
                );
            }
            PersonalFabricAction::WakeReport { target, outcome } => {
                let label = label_of(fabric, target);
                self.waker
                    .borrow_mut()
                    .reported(&envelope.source, target, *outcome, label);
                self.tell(shared);
            }
            PersonalFabricAction::WakeProbe { nonce } => {
                answer(
                    fabric,
                    &envelope.source,
                    PersonalFabricAction::WakeProbeReply { nonce: *nonce },
                );
            }
            PersonalFabricAction::WakeProbeReply { nonce } => {
                let step = self.waker.borrow_mut().answered(&envelope.source, *nonce);
                if let Some(step) = step {
                    self.carry_out(shared, fabric, step);
                }
            }
            _ => return false,
        }
        true
    }

    /// Wake the device the person named, by its id or its label: this device
    /// sends the packets when it shares the sleeping device's network, else
    /// asks the device that does.
    ///
    /// # Errors
    ///
    /// Returns why it cannot be woken, in the person's words: no device by
    /// that name, none of them awake on its network, or a signal that did not
    /// leave.
    pub(crate) fn request(&self, shared: &Shared, name: &str) -> Result<WakePlan, String> {
        let Some(fabric) = &shared.fabric else {
            return Err("this session has no device fabric".to_owned());
        };
        let (local, _) = fabric
            .local_device_fabric_identity()
            .ok_or_else(|| "this device has no identity in the person's devices".to_owned())?;
        let converged = fabric.device_fabric();
        let target =
            wake::resolve_device(&converged, name).map_err(|refusal| refusal.to_string())?;
        let local_routes = self.publish(fabric);
        let plan = wake::plan_wake(&converged, &local, &local_routes, &target, |device| {
            fabric.online_device_ids().contains(device)
        })
        .map_err(|refusal| refusal.to_string())?;
        if plan.waker != local {
            fabric
                .try_send_personal_fabric_action(
                    &plan.waker,
                    PersonalFabricAction::WakeDevice {
                        target: target.clone(),
                    },
                )
                .map_err(|error| {
                    format!(
                        "{} could not be asked to wake {}: {error}",
                        label_of(fabric, &plan.waker),
                        label_of(fabric, &target)
                    )
                })?;
            self.waker
                .borrow_mut()
                .asked(target, plan.waker.clone(), Instant::now());
            return Ok(plan);
        }
        match self.wake_from_here(fabric, &target, None) {
            WakeOutcome::Sent { .. } => Ok(plan),
            _ => Err(format!(
                "could not send a wake-up signal to {} on {}",
                label_of(fabric, &target),
                plan.subnet
            )),
        }
    }

    /// Send this machine's magic packets for `target` and start watching for
    /// it on behalf of `requester`.
    fn wake_from_here(
        &self,
        fabric: &SharedLink,
        target: &DeviceId,
        requester: Option<DeviceId>,
    ) -> WakeOutcome {
        let converged = fabric.device_fabric();
        let routes = self.publish(fabric);
        let packets = wake::magic_packets_for(&converged, &routes, target);
        let Some((_, subnet)) = packets.first().copied() else {
            return WakeOutcome::NotOnTargetLan;
        };
        let mut sent = false;
        for (mac, subnet) in &packets {
            match self.packets.send(*mac, *subnet) {
                Ok(()) => sent = true,
                Err(error) => {
                    tracing::warn!(%error, %subnet, "a wake-up signal could not be sent");
                }
            }
        }
        if !sent {
            return WakeOutcome::SendFailed;
        }
        self.waker
            .borrow_mut()
            .started(target.clone(), requester, Instant::now());
        WakeOutcome::Sent { subnet }
    }

    /// Do the next thing a watch asks for: ask the woken device whether it is
    /// back, or tell whoever asked how it ended.
    fn carry_out(&self, shared: &Shared, fabric: &SharedLink, step: WatchStep) {
        match step {
            WatchStep::Probe { target, nonce } => {
                // Unanswered until the target's session is up again.
                let _ = fabric.try_send_personal_fabric_action(
                    &target,
                    PersonalFabricAction::WakeProbe { nonce },
                );
            }
            WatchStep::Finished {
                target,
                requester: Some(requester),
                outcome,
            } => {
                answer(
                    fabric,
                    &requester,
                    PersonalFabricAction::WakeReport { target, outcome },
                );
            }
            finished @ WatchStep::Finished { .. } => {
                let label = match &finished {
                    WatchStep::Finished { target, .. } => label_of(fabric, target),
                    WatchStep::Probe { .. } => return,
                };
                self.waker.borrow_mut().tell_step(finished, label);
                self.tell(shared);
            }
        }
    }

    /// This machine's wake routes, re-read at most every
    /// [`ROUTE_SCAN_INTERVAL`], offered to the person's devices.
    fn publish(&self, fabric: &SharedLink) -> Vec<WakeRoute> {
        let now = Instant::now();
        let fresh = self
            .scanned
            .borrow()
            .is_some_and(|at| now.saturating_duration_since(at) < ROUTE_SCAN_INTERVAL);
        if !fresh {
            let routes = wake::remember_subnets(self.interfaces.routes(), &self.routes.borrow());
            *self.routes.borrow_mut() = routes;
            *self.scanned.borrow_mut() = Some(now);
        }
        let routes = self.routes.borrow().clone();
        fabric.share_wake_routes(routes.clone());
        routes
    }

    /// Tell the person how their requests to wake a device ended.
    fn tell(&self, shared: &Shared) {
        let outcomes = self.waker.borrow_mut().outcomes();
        if outcomes.is_empty() {
            return;
        }
        let notifications = shared.notifications.borrow();
        for outcome in &outcomes {
            tell(&notifications, outcome);
        }
    }
}

/// Send a wake answer to the device that asked.
fn answer(fabric: &SharedLink, target: &DeviceId, action: PersonalFabricAction) {
    if let Err(error) = fabric.try_send_personal_fabric_action(target, action) {
        tracing::debug!(%error, device = target.as_str(), "a wake answer did not leave");
    }
}

/// Tell the person how one request ended, if the outcome is one to tell.
fn tell(notifications: &SessionNotifications, event: &WakeEvent) {
    if let Some(notice) = crate::notices::wake_notice(event) {
        notifications.tell(notice);
    }
}

/// The label the person knows `device` by.
fn label_of(fabric: &SharedLink, device: &DeviceId) -> String {
    fabric.device_fabric().snapshots().get(device).map_or_else(
        || device.as_str().to_owned(),
        |device| device.presence.label.clone(),
    )
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
fn ipv4_subnets() -> std::collections::BTreeMap<String, Ipv4Subnet> {
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
