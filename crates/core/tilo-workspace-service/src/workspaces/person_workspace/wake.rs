//! Waking one of the person's sleeping devices from another of theirs.
//!
//! A device that sleeps off the network can still be woken by a magic packet: six `0xFF` bytes
//! then the target's MAC sixteen times, broadcast on the LAN it last sat on. Only a device on
//! that LAN can send it, so the person's devices cooperate through the record of their devices:
//! each keeps the interfaces it can be woken through as its fact [`WAKE_FACT`] ([`WakeRoute`]), a
//! wake is asked by writing it into the record ([`Devices::wake_device`]), every daemon of theirs
//! that shares a LAN with the target sends the packets and says so ([`Devices::wake_sent`]), and
//! the target says it is back once its daemon reads the ask on its return ([`Devices::woke`]).
//! How each wake stands ([`WakeState`]) is read from the record alone, on any device of theirs.
//!
//! Nothing assumes a kind of interface wakes a machine: only what the device itself reports
//! enabled is a wake route, because a Wi-Fi chip can list `WoWLAN` while the machine around it
//! does not wake from it.

use serde::{Deserialize, Serialize};

use super::devices::{Device, Devices, KeyKind, WakeAsk};

/// The fact under which a device keeps the interfaces it can be woken through: its
/// [`WakeRoute`]s, postcard in hex.
pub const WAKE_FACT: &str = "wake";
/// More network interfaces than any one device wakes through.
pub const MAX_WAKE_ROUTES: usize = 8;
/// The port a magic packet goes to.
pub const MAGIC_PACKET_PORT: u16 = 9;
/// How long a woken device has to come back before the wake counts as failed, ns.
pub const WAKE_REJOIN_NS: i64 = 150_000_000_000;
/// How long a wake waits for a device of the person's to send its packets, ns: one that no
/// device on the target's LAN took up by then is not going to be.
pub const WAKE_SEND_NS: i64 = 30_000_000_000;

/// The kind of network interface a device can be woken through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeInterfaceKind {
    Ethernet,
    WiFi,
}

/// An IPv4 network in canonical form: no host bits set beyond the prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Ipv4Subnet {
    pub network: [u8; 4],
    pub prefix: u8,
}

impl Ipv4Subnet {
    /// The subnet `address/prefix` lies in, or `None` for a prefix that names no broadcast domain
    /// (wider than /8 or a single host).
    #[must_use]
    pub fn containing(address: std::net::Ipv4Addr, prefix: u8) -> Option<Self> {
        if !(8..=30).contains(&prefix) {
            return None;
        }
        let mask = u32::MAX.checked_shl(32_u32.checked_sub(u32::from(prefix))?)?;
        Some(Self {
            network: (u32::from(address) & mask).to_be_bytes(),
            prefix,
        })
    }

    #[must_use]
    pub fn is_canonical(self) -> bool {
        Self::containing(std::net::Ipv4Addr::from(self.network), self.prefix) == Some(self)
    }

    /// The subnet's directed broadcast address.
    #[must_use]
    pub fn broadcast(self) -> std::net::Ipv4Addr {
        let host = u32::MAX.checked_shr(u32::from(self.prefix)).unwrap_or(0);
        std::net::Ipv4Addr::from(u32::from_be_bytes(self.network) | host)
    }
}

impl std::fmt::Display for Ipv4Subnet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{}",
            std::net::Ipv4Addr::from(self.network),
            self.prefix
        )
    }
}

impl std::str::FromStr for Ipv4Subnet {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, ()> {
        let (address, prefix) = text.split_once('/').ok_or(())?;
        let subnet = Self {
            network: address
                .parse::<std::net::Ipv4Addr>()
                .map_err(|_| ())?
                .octets(),
            prefix: prefix.parse().map_err(|_| ())?,
        };
        subnet.is_canonical().then_some(subnet).ok_or(())
    }
}

/// One network interface of a device, as the network sees it: what another device of the
/// person's on the same LAN needs to wake it with a magic packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeRoute {
    /// The MAC the network sees, which for Wi-Fi is the per-network one.
    pub mac: [u8; 6],
    pub kind: WakeInterfaceKind,
    /// Whether the interface wakes the device on a magic packet.
    pub magic_packet: bool,
    /// The IPv4 subnet the interface was last seen on.
    pub subnet: Option<Ipv4Subnet>,
}

impl WakeRoute {
    /// A unicast, non-zero MAC, on a subnet in canonical form where it names one.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.mac != [0; 6]
            && self.mac.first().is_some_and(|octet| octet & 1 == 0)
            && self.subnet.is_none_or(Ipv4Subnet::is_canonical)
    }
}

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

/// Keep the subnet an interface was last seen on while it has none now, so a device that went to
/// sleep off its network stays wakeable once back.
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

/// `routes` as a device keeps them in its entry: postcard in hex.
#[must_use]
pub fn encoded(routes: &[WakeRoute]) -> String {
    hex::encode(postcard::to_allocvec(routes).expect("wake routes encode"))
}

/// The interfaces `device` keeps that it can be woken through, where they hold: no more than a
/// device has, each one an interface could be, none named twice. None when it keeps none.
#[must_use]
pub fn routes(device: &Device) -> Vec<WakeRoute> {
    let Some(value) = device.facts.get(WAKE_FACT) else {
        return Vec::new();
    };
    let decoded = hex::decode(value)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            postcard::from_bytes::<Vec<WakeRoute>>(&bytes).map_err(|error| error.to_string())
        });
    let routes = match decoded {
        Ok(routes) => routes,
        Err(error) => {
            tracing::warn!(%error, "a device's wake routes in the record do not read");
            return Vec::new();
        }
    };
    let mut macs = routes.iter().map(|route| route.mac).collect::<Vec<_>>();
    macs.sort_unstable();
    macs.dedup();
    if routes.len() > MAX_WAKE_ROUTES
        || macs.len() != routes.len()
        || !routes.iter().all(WakeRoute::valid)
    {
        tracing::warn!("a device's wake routes in the record do not hold");
        return Vec::new();
    }
    routes
}

/// Whether a magic packet wakes `device` through one of its interfaces.
#[must_use]
pub fn wakeable(device: &Device) -> bool {
    routes(device).iter().any(|route| route.magic_packet)
}

/// Whether two devices were last seen on a common IPv4 subnet.
#[must_use]
pub fn shares_network(own: &[WakeRoute], other: &[WakeRoute]) -> bool {
    own.iter()
        .filter_map(|route| route.subnet)
        .any(|subnet| other.iter().any(|route| route.subnet == Some(subnet)))
}

/// The subnets a magic packet can wake the device keeping `routes` on, in its own order.
fn wakeable_subnets(routes: &[WakeRoute]) -> Vec<Ipv4Subnet> {
    let mut subnets = Vec::new();
    for route in routes {
        if let (true, Some(subnet)) = (route.magic_packet, route.subnet)
            && !subnets.contains(&subnet)
        {
            subnets.push(subnet);
        }
    }
    subnets
}

/// Why a device cannot be woken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WakeRefusal {
    /// No device of the person's is called that.
    UnknownDevice(String),
    /// More than one device of theirs is.
    AmbiguousDevice(String),
    /// The device that would be woken is the one asking.
    LocalDevice,
    /// The device keeps no interface that wakes on a magic packet.
    NoWakeRoute(String),
    /// No other device of the person's was last on any subnet the target wakes on.
    NoOtherDevice(Vec<Ipv4Subnet>),
}

impl std::fmt::Display for WakeRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownDevice(name) => write!(formatter, "no device of yours is called {name}"),
            Self::AmbiguousDevice(name) => write!(
                formatter,
                "{name} names more than one of your devices; use its device id"
            ),
            Self::LocalDevice => formatter.write_str("this device is already awake"),
            Self::NoWakeRoute(label) => {
                write!(formatter, "{label} can't be woken over the network")
            }
            Self::NoOtherDevice(subnets) => {
                let subnets = subnets
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" or ");
                write!(formatter, "no other device of yours is on {subnets}")
            }
        }
    }
}

impl std::error::Error for WakeRefusal {}

/// The person's devices `record` keeps under `person`'s root: their devices, not their agents'
/// or scripts' keys, nor one their root took back. Each with its key and its label.
fn devices<'a>(
    record: &'a Devices,
    person: &[u8; 32],
) -> impl Iterator<Item = (String, String, Device)> + 'a {
    record
        .listed(person)
        .into_iter()
        .filter(|listed| {
            listed.device.kind == KeyKind::Device && !listed.device.revoked(&listed.cert)
        })
        .map(|listed| {
            let label = listed.device.called(&listed.cert);
            (listed.key, label, listed.device)
        })
}

/// The device `name` names among `person`'s devices in `record`: its key, or else the one device
/// whose label matches it ignoring case.
///
/// # Errors
/// `name` names no device of theirs, or more than one.
pub fn resolve(record: &Devices, person: &[u8; 32], name: &str) -> Result<String, WakeRefusal> {
    let name = name.trim();
    if devices(record, person).any(|(key, _, _)| key == name) {
        return Ok(name.to_owned());
    }
    let mut matches = devices(record, person)
        .filter(|(_, label, _)| label.eq_ignore_ascii_case(name))
        .map(|(key, _, _)| key);
    match (matches.next(), matches.next()) {
        (Some(device), None) => Ok(device),
        (Some(_), Some(_)) => Err(WakeRefusal::AmbiguousDevice(name.to_owned())),
        (None, _) => Err(WakeRefusal::UnknownDevice(name.to_owned())),
    }
}

/// The action that asks the person's devices to wake the device `name` names, asked by `local`
/// (hex) at `now_ns`: one whose interfaces some other device of theirs was last on a LAN with.
///
/// # Errors
/// Why it cannot be woken, in the person's words.
pub fn asking(
    record: &Devices,
    person: &[u8; 32],
    local: &str,
    name: &str,
    now_ns: i64,
) -> Result<tilo_resource::Invoke<Devices>, WakeRefusal> {
    let target = resolve(record, person, name)?;
    if target == local {
        return Err(WakeRefusal::LocalDevice);
    }
    let (_, label, device) = devices(record, person)
        .find(|(key, _, _)| *key == target)
        .ok_or_else(|| WakeRefusal::UnknownDevice(target.clone()))?;
    let subnets = wakeable_subnets(&routes(&device));
    if subnets.is_empty() {
        return Err(WakeRefusal::NoWakeRoute(label));
    }
    let reached = devices(record, person).any(|(key, _, other)| {
        key != target
            && routes(&other)
                .iter()
                .any(|route| route.subnet.is_some_and(|subnet| subnets.contains(&subnet)))
    });
    if !reached {
        return Err(WakeRefusal::NoOtherDevice(subnets));
    }
    Ok(Devices::WAKE_DEVICE.with((target, local.to_owned(), now_ns)))
}

/// The packets a device on `own` interfaces sends to wake the device `record` keeps under
/// `target`: one per interface of the target that wakes on a magic packet, on a subnet this
/// device is on.
#[must_use]
pub fn packets(record: &Devices, own: &[WakeRoute], target: &str) -> Vec<([u8; 6], Ipv4Subnet)> {
    record
        .devices
        .get(&tilo_resource::Id::from_raw(target.to_owned()))
        .map(routes)
        .unwrap_or_default()
        .into_iter()
        .filter(|route| route.magic_packet)
        .filter_map(|route| Some((route.mac, route.subnet?)))
        .filter(|(_, subnet)| own.iter().any(|route| route.subnet == Some(*subnet)))
        .collect()
}

/// What the device `own` (hex), on the interfaces `routes`, does about the wakes `record` holds at
/// `now_ns`: it sends the packets of each wake still waiting for them that it shares a LAN for,
/// through `send` (whether one left), and says it is back from a wake of itself that is still
/// waited for. `done` keeps the wakes it already took up, so a pass before the record reads its
/// own answer back sends nothing twice. The writes it makes, in order.
pub fn answering(
    record: &Devices,
    own: &str,
    routes: &[WakeRoute],
    now_ns: i64,
    done: &mut std::collections::BTreeSet<(String, i64)>,
    mut send: impl FnMut([u8; 6], Ipv4Subnet) -> bool,
) -> Vec<tilo_resource::Invoke<Devices>> {
    let mut writes = Vec::new();
    for (target, ask) in record.wakes.iter() {
        let target = target.as_str();
        let waited = now_ns.saturating_sub(ask.asked_ns) <= WAKE_SEND_NS + WAKE_REJOIN_NS;
        if !waited || ask.awake_ns > 0 || done.contains(&(target.to_owned(), ask.asked_ns)) {
            continue;
        }
        if target == own {
            done.insert((target.to_owned(), ask.asked_ns));
            writes.push(Devices::WOKE.with((target.to_owned(), ask.asked_ns, now_ns)));
            continue;
        }
        if ask.sent_ns > 0 || now_ns.saturating_sub(ask.asked_ns) > WAKE_SEND_NS {
            continue;
        }
        let packets = packets(record, routes, target);
        let Some((_, subnet)) = packets.first().copied() else {
            continue;
        };
        done.insert((target.to_owned(), ask.asked_ns));
        let mut sent = false;
        for (mac, subnet) in packets {
            sent |= send(mac, subnet);
        }
        if sent {
            writes.push(Devices::WAKE_SENT.with((
                target.to_owned(),
                ask.asked_ns,
                own.to_owned(),
                subnet.to_string(),
                now_ns,
            )));
        }
    }
    done.retain(|(target, asked_ns)| {
        record
            .wakes
            .get(&tilo_resource::Id::from_raw(target.clone()))
            .is_some_and(|ask| ask.asked_ns == *asked_ns)
    });
    writes
}

/// How one wake stands, as the record says it at `now_ns`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WakeState {
    /// Asked, and no device of the person's sent its packets yet.
    Asked,
    /// A device of theirs sent the packets on `subnet`; the target is not back yet.
    Sent { subnet: String },
    /// The target is back.
    Awake,
    /// The target did not come back in time after the packets went out.
    NotAwake,
    /// No device of theirs on the target's LAN sent the packets in time.
    Unsent,
}

impl WakeAsk {
    /// How this wake stands at `now_ns`.
    #[must_use]
    pub fn state(&self, now_ns: i64) -> WakeState {
        if self.awake_ns > 0 {
            WakeState::Awake
        } else if self.sent_ns > 0 {
            if now_ns.saturating_sub(self.sent_ns) > WAKE_REJOIN_NS {
                WakeState::NotAwake
            } else {
                WakeState::Sent {
                    subnet: self.subnet.clone(),
                }
            }
        } else if now_ns.saturating_sub(self.asked_ns) > WAKE_SEND_NS {
            WakeState::Unsent
        } else {
            WakeState::Asked
        }
    }

    /// When how it stands next changes without anything written, ns since the epoch: `None` once
    /// it is settled.
    #[must_use]
    pub fn next_change_ns(&self) -> Option<i64> {
        if self.awake_ns > 0 {
            None
        } else if self.sent_ns > 0 {
            Some(self.sent_ns.saturating_add(WAKE_REJOIN_NS))
        } else {
            Some(self.asked_ns.saturating_add(WAKE_SEND_NS))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Replica;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    fn device(root: &ed25519_dalek::SigningKey, seed: u8, label: &str) -> DeviceCert {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.to_owned(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).expect("the root signs");
        cert
    }

    fn route(mac: u8, subnet: &str, magic_packet: bool) -> WakeRoute {
        WakeRoute {
            mac: [0x02, 0, 0, 0, 0, mac],
            kind: WakeInterfaceKind::Ethernet,
            magic_packet,
            subnet: Some(subnet.parse().expect("a subnet")),
        }
    }

    fn keeps(record: &mut Replica<Devices>, cert: &DeviceCert, routes: &[WakeRoute]) {
        record
            .apply(Devices::KEEP_FACT.with((
                hex::encode(cert.device_public_key),
                hex::encode(postcard::to_allocvec(cert).unwrap()),
                WAKE_FACT.to_owned(),
                encoded(routes),
            )))
            .expect("a device keeps its routes");
    }

    #[test]
    fn a_wake_is_asked_sent_and_answered_through_the_record() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let person = root.verifying_key().to_bytes();
        let (laptop, desktop, server) = (
            device(&root, 2, "Laptop"),
            device(&root, 3, "Desktop"),
            device(&root, 4, "Server"),
        );
        let laptop_key = hex::encode(laptop.device_public_key);
        let server_key = hex::encode(server.device_public_key);
        let mut record = Replica::<Devices>::new();
        keeps(&mut record, &laptop, &[route(1, "10.1.0.0/16", false)]);
        keeps(&mut record, &desktop, &[route(2, "192.168.1.0/24", false)]);
        keeps(&mut record, &server, &[route(3, "192.168.1.0/24", true)]);
        let state = record.state().unwrap();

        assert_eq!(
            asking(&state, &person, &laptop_key, "Laptop", 10).err(),
            Some(WakeRefusal::LocalDevice)
        );
        assert_eq!(
            asking(&state, &person, &laptop_key, "Desktop", 10).err(),
            Some(WakeRefusal::NoWakeRoute("Desktop".into())),
            "a device that wakes on no magic packet cannot be woken"
        );
        assert!(matches!(
            asking(&state, &person, &laptop_key, "Phone", 10),
            Err(WakeRefusal::UnknownDevice(_))
        ));

        // The laptop is away from the server's LAN, but the desktop is on it.
        let ask = asking(&state, &person, &laptop_key, "server", 10).expect("it can be woken");
        record.apply(ask).expect("the ask is kept");
        let state = record.state().unwrap();
        let kept = state
            .wakes
            .get(&tilo_resource::Id::from_raw(server_key.clone()));
        let kept = kept.expect("the wake is in the record").clone();
        assert_eq!(kept.state(11), WakeState::Asked);
        assert_eq!(kept.state(10 + WAKE_SEND_NS + 1), WakeState::Unsent);

        assert!(
            packets(&state, &[route(1, "10.1.0.0/16", false)], &server_key).is_empty(),
            "the laptop shares no LAN with the server and sends nothing"
        );
        let sent = packets(&state, &[route(2, "192.168.1.0/24", false)], &server_key);
        assert_eq!(
            sent,
            [([0x02, 0, 0, 0, 0, 3], "192.168.1.0/24".parse().unwrap())]
        );

        let desktop_key = hex::encode(desktop.device_public_key);
        record
            .apply(Devices::WAKE_SENT.with((
                server_key.clone(),
                10,
                desktop_key,
                "192.168.1.0/24".to_owned(),
                20,
            )))
            .expect("the desktop says it sent the packets");
        let kept = record
            .state()
            .unwrap()
            .wakes
            .values()
            .next()
            .unwrap()
            .clone();
        assert_eq!(
            kept.state(21),
            WakeState::Sent {
                subnet: "192.168.1.0/24".into()
            }
        );
        assert_eq!(kept.state(20 + WAKE_REJOIN_NS + 1), WakeState::NotAwake);

        record
            .apply(Devices::WOKE.with((server_key.clone(), 10, 30)))
            .expect("the server says it is back");
        let kept = record
            .state()
            .unwrap()
            .wakes
            .values()
            .next()
            .unwrap()
            .clone();
        assert_eq!(kept.state(i64::MAX), WakeState::Awake);
        assert_eq!(kept.next_change_ns(), None);

        assert!(
            record
                .apply(Devices::WOKE.with((server_key, 9, 31)))
                .is_ok(),
            "an answer to an older ask changes nothing"
        );
        assert_eq!(
            record
                .state()
                .unwrap()
                .wakes
                .values()
                .next()
                .unwrap()
                .awake_ns,
            30
        );
    }

    #[test]
    fn routes_that_do_not_hold_read_as_none() {
        let mut device = Device::default();
        device
            .facts
            .insert(WAKE_FACT.into(), encoded(&[route(1, "10.0.0.0/8", true)]));
        assert!(wakeable(&device));
        let twice = [route(1, "10.0.0.0/8", true), route(1, "10.0.0.0/8", true)];
        device.facts.insert(WAKE_FACT.into(), encoded(&twice));
        assert!(routes(&device).is_empty(), "an interface named twice");
        device.facts.insert(WAKE_FACT.into(), "zz".into());
        assert!(routes(&device).is_empty(), "a value that is no hex");
        assert_eq!(parse_mac("02:00:00:00:00:01"), Some([2, 0, 0, 0, 0, 1]));
        assert_eq!(parse_mac("01:00:00:00:00:01"), None, "a multicast MAC");
        assert_eq!(magic_packet([7; 6])[6..12], [7; 6]);
    }
}
