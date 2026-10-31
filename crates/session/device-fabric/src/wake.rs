//! Waking one of the person's sleeping devices from their fabric.
//!
//! A device that sleeps off the network can still be woken by a magic packet:
//! six `0xFF` bytes then the target's MAC sixteen times, broadcast on the LAN
//! it last sat on. Only a device sharing that LAN can send it, so the person's
//! other devices cooperate: the one that shares it sends the packet, and any
//! device may ask for the wake ([`crate::PersonalFabricAction::WakeDevice`]).
//!
//! What each device needs known here is pure and decided by the fabric the
//! session already keeps: which device a name or id names, which of them can
//! wake the target ([`plan_wake`]), what to send once it is this device's turn
//! ([`magic_packets_for`]), and whether the target came back ([`Waker`]). The
//! socket, the clock and the session's own interfaces are the caller's: a
//! session test sends through a [`MagicPackets`] of its own.
//!
//! Nothing assumes a kind of interface wakes a machine: only what the device
//! itself reports enabled is a wake route, because a Wi-Fi chip can list
//! `WoWLAN` while the machine around it does not wake from it.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use crate::{DeviceFabric, DeviceId, Ipv4Subnet, MAX_WAKE_ROUTES, WakeOutcome, WakeRoute};

/// The port a magic packet goes to.
pub const MAGIC_PACKET_PORT: u16 = 9;
/// How long the waker watches for the target to come back.
pub const WAKE_REJOIN_TIMEOUT: Duration = Duration::from_secs(150);
/// How often the waker probes the target it just woke.
pub const WAKE_PROBE_INTERVAL: Duration = Duration::from_secs(3);
/// How long a device that asked another one to wake a target waits to hear
/// how it went.
pub const WAKE_REPORT_TIMEOUT: Duration = Duration::from_secs(180);
/// How often a device re-reads its own interfaces for its wake routes.
pub const ROUTE_SCAN_INTERVAL: Duration = Duration::from_secs(10);
/// Watches one device keeps at once.
pub const MAX_PENDING_WAKES: usize = 16;
/// Outcomes waiting to be told to the person.
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

/// Why a device cannot be woken right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WakeRefusal {
    /// No device of the person's is called that.
    UnknownDevice(String),
    /// More than one device of theirs is.
    AmbiguousDevice(String),
    /// The device that would be woken is this one.
    LocalDevice,
    /// The device reports no interface that wakes on a magic packet.
    NoWakeRoute(String),
    /// No device awake on any subnet the target can be woken on.
    NoAwakeSibling(Vec<Ipv4Subnet>),
}

impl std::fmt::Display for WakeRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownDevice(name) => {
                write!(formatter, "no device of yours is called {name}")
            }
            Self::AmbiguousDevice(name) => write!(
                formatter,
                "{name} names more than one of your devices; use its device id"
            ),
            Self::LocalDevice => formatter.write_str("this device is already awake"),
            Self::NoWakeRoute(label) => {
                write!(formatter, "{label} can't be woken over the network")
            }
            Self::NoAwakeSibling(subnets) => {
                let subnets = subnets
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" or ");
                write!(formatter, "no awake device on {subnets}")
            }
        }
    }
}

impl std::error::Error for WakeRefusal {}

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
/// device reports enabled.
fn wakes(route: &WakeRoute) -> bool {
    route.magic_packet
}

fn on_subnet(routes: &[WakeRoute], subnet: Ipv4Subnet) -> bool {
    routes.iter().any(|route| route.subnet == Some(subnet))
}

/// Pick the device that wakes `target`: this device when it is on one of the
/// target's subnets, else the first awake sibling on one, in subnet order and
/// then device order.
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

/// The packets this device sends to wake `target`: one per wakeable interface
/// of the target on a subnet this device is on.
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

/// Keep the subnet an interface was last seen on while it has none now, so a
/// device that went to sleep off its network stays wakeable once back.
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
    current.truncate(MAX_WAKE_ROUTES);
    current
}

/// Where a magic packet goes: the socket on a real session, a recorder in a
/// test, so nothing in a test broadcasts.
pub trait MagicPackets: Send + Sync {
    /// Broadcast one magic packet for `mac` on `subnet`.
    ///
    /// # Errors
    ///
    /// Returns the socket error.
    fn send(&self, mac: [u8; 6], subnet: Ipv4Subnet) -> std::io::Result<()>;
}

/// The real socket: a broadcast to the subnet's address on the magic packet
/// port.
#[derive(Clone, Copy, Debug, Default)]
pub struct UdpMagicPackets;

impl MagicPackets for UdpMagicPackets {
    fn send(&self, mac: [u8; 6], subnet: Ipv4Subnet) -> std::io::Result<()> {
        let socket = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))?;
        socket.set_broadcast(true)?;
        socket.send_to(&magic_packet(mac), (subnet.broadcast(), MAGIC_PACKET_PORT))?;
        Ok(())
    }
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
    /// Ask the target whether it is back yet.
    Probe { target: DeviceId, nonce: [u8; 16] },
    /// The target answered, or gave the wait up.
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
    /// Start watching `target`, replacing an older watch for it. `false` when
    /// too many wakes are already in flight.
    pub fn start(&mut self, target: DeviceId, requester: Option<DeviceId>, now: Instant) -> bool {
        if !self.watching.contains_key(&target) && self.watching.len() >= MAX_PENDING_WAKES {
            return false;
        }
        self.watching.insert(
            target,
            Watch {
                requester,
                started: now,
                last_probe: None,
                nonce: crate::random::nonzero::<16>(),
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

    /// `target` is back among the person's devices: the watch for it is done
    /// without asking it anything.
    pub fn rejoined(&mut self, target: &DeviceId) -> Option<WatchStep> {
        let watch = self.watching.remove(target)?;
        Some(WatchStep::Finished {
            target: target.clone(),
            requester: watch.requester,
            outcome: WakeOutcome::Rejoined,
        })
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

/// This device's side of the wakes it takes part in: the devices it is
/// watching for, the wakes it asked another device to perform, and the
/// outcomes the person has not been told yet.
#[derive(Debug, Default)]
pub struct Waker {
    watch: WakeWatch,
    /// Wakes this device asked another one to perform: target → (waker, asked
    /// at).
    requested: BTreeMap<DeviceId, (DeviceId, Instant)>,
    events: VecDeque<WakeEvent>,
}

impl Waker {
    /// Start watching `target` on behalf of `requester`, once this device sent
    /// its packets. `false` when too many wakes are in flight.
    pub fn started(&mut self, target: DeviceId, requester: Option<DeviceId>, now: Instant) -> bool {
        self.watch.start(target, requester, now)
    }

    /// Remember that this device asked `waker` to wake `target`.
    pub fn asked(&mut self, target: DeviceId, waker: DeviceId, now: Instant) {
        self.requested.insert(target, (waker, now));
    }

    /// The waker of `target` reports how it went. Whether the report answers
    /// a wake this device asked for; either way the person is told.
    pub fn reported(
        &mut self,
        from: &DeviceId,
        target: &DeviceId,
        outcome: WakeOutcome,
        label: String,
    ) -> bool {
        let answered = self
            .requested
            .get(target)
            .is_some_and(|(waker, _)| waker == from);
        if answered && !matches!(outcome, WakeOutcome::Sent { .. }) {
            self.requested.remove(target);
        }
        self.tell(WakeEvent {
            target: target.clone(),
            label,
            outcome,
        });
        answered
    }

    /// The watches the devices among `online` finish: a device back on the
    /// person's fabric is awake.
    pub fn rejoins(&mut self, online: &std::collections::BTreeSet<DeviceId>) -> Vec<WatchStep> {
        online
            .iter()
            .filter_map(|device| self.watch.rejoined(device))
            .collect()
    }

    /// `from` answered a probe with `nonce`; the step to carry out when it was
    /// the device this one woke.
    pub fn answered(&mut self, from: &DeviceId, nonce: [u8; 16]) -> Option<WatchStep> {
        self.watch.answered(from, nonce)
    }

    /// Probes that are due, and watches that ran out of time.
    pub fn poll(&mut self, now: Instant) -> Vec<WatchStep> {
        self.watch.poll(now)
    }

    /// Forget wakes another device never reported, telling the person the
    /// target did not come back.
    pub fn abandoned(&mut self, now: Instant, label: impl Fn(&DeviceId) -> String) {
        let mut abandoned = Vec::new();
        self.requested.retain(|target, (_, asked)| {
            let waiting = now.saturating_duration_since(*asked) < WAKE_REPORT_TIMEOUT;
            if !waiting {
                abandoned.push(target.clone());
            }
            waiting
        });
        for target in abandoned {
            let label = label(&target);
            self.tell(WakeEvent {
                target,
                label,
                outcome: WakeOutcome::NotRejoined,
            });
        }
    }

    /// Tell the person what became of a watch that finished.
    pub fn tell_step(&mut self, step: WatchStep, label: String) {
        if let WatchStep::Finished {
            target, outcome, ..
        } = step
        {
            self.tell(WakeEvent {
                target,
                label,
                outcome,
            });
        }
    }

    fn tell(&mut self, event: WakeEvent) {
        if self.events.len() >= MAX_WAKE_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// The outcomes the person has not been told yet.
    pub fn outcomes(&mut self) -> Vec<WakeEvent> {
        self.events.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;
    use crate::{
        DeviceFabricSnapshot, DeviceKind, DevicePresence, FABRIC_SCHEMA_VERSION,
        FabricCapabilities, OwnerSession, WakeInterfaceKind,
    };
    use tilo_network_actor::fabric::FabricStamp;

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
            unlock: crate::unlock::UnlockFabricState::default(),
            speakers: crate::speaker_group::SpeakerFabricState::default(),
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
            subnet("192.168.231.11/24").broadcast(),
            Ipv4Addr::new(192, 168, 231, 255)
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
        let mut waker = Waker::default();
        assert!(waker.started(id("desk"), Some(id("phone")), start));
        let nonce = waker
            .poll(start)
            .first()
            .and_then(|step| match step {
                WatchStep::Probe { nonce, .. } => Some(*nonce),
                WatchStep::Finished { .. } => None,
            })
            .expect("a probe is due at once");
        assert!(
            waker.poll(start + Duration::from_secs(1)).is_empty(),
            "probes are paced"
        );
        assert!(
            waker.answered(&id("desk"), [8; 16]).is_none(),
            "a stale nonce"
        );
        assert!(
            waker.answered(&id("phone"), nonce).is_none(),
            "another device"
        );
        assert_eq!(
            waker.answered(&id("desk"), nonce),
            Some(WatchStep::Finished {
                target: id("desk"),
                requester: Some(id("phone")),
                outcome: WakeOutcome::Rejoined,
            })
        );
        assert!(waker.poll(start + WAKE_PROBE_INTERVAL).is_empty());

        // A device that is back among the person's devices is awake without
        // being asked.
        assert!(waker.started(id("tv"), None, start));
        assert_eq!(
            waker.rejoins(&std::collections::BTreeSet::from([id("tv")])),
            vec![WatchStep::Finished {
                target: id("tv"),
                requester: None,
                outcome: WakeOutcome::Rejoined,
            }]
        );

        assert!(waker.started(id("desk"), None, start));
        let _ = waker.poll(start);
        assert_eq!(
            waker.poll(start + WAKE_REJOIN_TIMEOUT),
            vec![WatchStep::Finished {
                target: id("desk"),
                requester: None,
                outcome: WakeOutcome::NotRejoined,
            }]
        );
    }

    #[test]
    fn a_wake_asked_of_another_device_is_settled_by_its_report() {
        let now = Instant::now();
        let mut waker = Waker::default();
        waker.asked(id("desk"), id("server"), now);
        assert!(
            !waker.reported(
                &id("phone"),
                &id("desk"),
                WakeOutcome::Rejoined,
                "Desk".into()
            ),
            "a report from a device that was not asked settles nothing"
        );
        assert!(waker.reported(
            &id("server"),
            &id("desk"),
            WakeOutcome::NotOnTargetLan,
            "Desk".into()
        ));
        let told = waker.outcomes();
        assert_eq!(told.len(), 2);
        assert_eq!(
            told.last()
                .map(|event| (event.label.as_str(), event.outcome)),
            Some(("Desk", WakeOutcome::NotOnTargetLan))
        );

        // A waker that never reports is given up on.
        waker.asked(id("tv"), id("server"), now);
        waker.abandoned(now + WAKE_REPORT_TIMEOUT, |device| {
            format!("device {}", device.as_str())
        });
        assert_eq!(
            waker.outcomes().first().map(|event| event.outcome),
            Some(WakeOutcome::NotRejoined)
        );
    }
}
