//! The device VPN's peer-to-peer half, on the device's one endpoint (docs/device-vpn.md).
//!
//! [`DeviceVpn`] runs beside the live relay and Nearby Send, for the daemon's life. It dials the
//! exit another of the person's devices serves — the daemon is the one process with the device
//! key, so only it can dial as this device — and it answers the tunnel on this device's endpoint
//! when the person lets their other devices route through this one. Either way it carries the
//! packets.
//!
//! A shell reaches it over one of its links ([`device_vpn::link`]): the session opens its TUN
//! devices through the root helper, which answers the seat's compositor alone, and hands the
//! packets to the link; the daemon hands them to the tunnel. So the tunnel runs where the endpoint
//! is and the TUN lives where the privilege is, and neither process needs what the other has.
//!
//! Who may route through this device is the person's own devices, as their record lists them,
//! less the ones taken back or expired — and only while a shell lends this device's exit TUN,
//! which it does while the person turned that exit on. A device the person stopped there is
//! stopped at once, and one they remove from their devices is cut by the exit's own recheck, which
//! judges every running session again.

use std::collections::{BTreeSet, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use device_vpn::link::{self, Control, Frame};
use device_vpn::net::{Ipv4Net, is_relay_address};
use device_vpn::protocol::{Carrier, MAX_CARRIER_PORTS, MAX_RELAYS};
use device_vpn::tunnel::dns::DnsRelay;
use device_vpn::tunnel::{ALPN, Authority, Decision, Exit, ExitDevices, PacketDevice, client};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_lock::MutexRecover as _;
use tilo_runtime::device::capabilities::{Devices, VpnServing};
use tilo_runtime::device::wake::ipv4_subnets;

/// How long the relays' names may take to resolve before the carrier leaves them out: opening the
/// tunnel never waits on a lookup.
const RELAY_LOOKUP: Duration = Duration::from_secs(2);
/// How often a lent exit link tells the shell who routes through this device.
const USERS_EVERY: Duration = Duration::from_secs(1);
/// Packets a lent exit link keeps before the oldest is dropped, as a network drops them.
const PACKETS_KEPT: usize = 256;

/// The device VPN, served on the device's endpoint and asked for over a shell's links.
struct DeviceVpn {
    endpoint: Endpoint,
    /// Who may route through this device, and whether its exit is on.
    allowed: Allowed,
    /// The exit that answers the person's other devices.
    exit: Exit<Allowed, LentExit>,
    /// This device's exit TUN, while a shell lends it.
    lent: Arc<Mutex<Option<Arc<LinkPump>>>>,
}

/// Serve the device VPN on `endpoint`, admitting `devices`.
pub fn serving(endpoint: Endpoint, devices: Devices) -> VpnServing {
    let vpn = Arc::new(DeviceVpn::new(endpoint, devices));
    let links = Arc::clone(&vpn);
    VpnServing {
        protocols: vpn.protocols(),
        links: Arc::new(move |stream| {
            let vpn = Arc::clone(&links);
            Box::pin(async move { vpn.serve_link(stream).await })
        }),
    }
}

impl DeviceVpn {
    /// Serve the device VPN on `endpoint`, admitting `devices`.
    fn new(endpoint: Endpoint, devices: Devices) -> Self {
        let allowed = Allowed(Arc::new(Who {
            devices,
            on: AtomicBool::new(false),
        }));
        let lent = Arc::new(Mutex::new(None));
        let exit = Exit::new(
            allowed.clone(),
            LentExit(Arc::clone(&lent)),
            DnsRelay::system(),
            || {},
        );
        Self {
            endpoint,
            allowed,
            exit,
            lent,
        }
    }

    /// What this device answers on its endpoint beside its other protocols: the tunnel another of
    /// the person's devices dials to route through this one.
    fn protocols(&self) -> Vec<(&'static [u8], Box<dyn iroh::protocol::DynProtocolHandler>)> {
        vec![(
            ALPN,
            Box::<dyn iroh::protocol::DynProtocolHandler>::from(self.exit.clone()),
        )]
    }

    /// Serve one link a shell of this device opened until it ends: a route the daemon dials and
    /// carries, a probe of what a device offers, or the shell's exit TUN, lent so the person's
    /// other devices may route through this one.
    async fn serve_link(&self, stream: tilo_socket::tokio::Stream) {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let ask = match link::read(&mut reader).await {
            Ok(Frame::Control(control)) => control,
            Ok(Frame::Packet(_)) => {
                tracing::debug!("a device VPN link opened with a packet, not an ask");
                return;
            }
            Err(error) => {
                tracing::info!(%error, "a device VPN link ended before it asked");
                return;
            }
        };
        let answered = match ask {
            Control::Probe { exit, at } => {
                self.probe(&exit, at, &mut writer).await;
                return;
            }
            Control::Through { exit, at } => self.through(&exit, at, reader, writer).await,
            Control::Exit => self.lend(reader, writer).await,
            other => Err(io::Error::other(format!("no ask a shell makes: {other:?}"))),
        };
        if let Err(error) = answered {
            tracing::info!(%error, "a device VPN link ended unserved");
        }
    }

    /// What `exit` offers another of the person's devices, without routing through it.
    async fn probe<W>(&self, exit: &str, at: Option<EndpointAddr>, writer: &mut W)
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        let said = match exit_id(exit) {
            Some(exit) => match client::probe(&self.endpoint, reach(exit, at)).await {
                Ok(offer) => Control::Offer {
                    mtu: offer.mtu,
                    offer,
                    carrier: carrier(&self.endpoint).await,
                },
                Err(error) => refusal(&error),
            },
            None => Control::Failed("no device is named to probe".to_owned()),
        };
        if let Err(error) = link::write_control(writer, &said).await {
            tracing::info!(%error, "a probe's answer did not reach the shell");
        }
    }

    /// Dial `exit` and carry this device's packets through it until the link ends.
    async fn through<R, W>(
        &self,
        exit: &str,
        at: Option<EndpointAddr>,
        reader: R,
        writer: W,
    ) -> io::Result<()>
    where
        R: tokio::io::AsyncRead + Unpin + Send + Sync + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + Sync + 'static,
    {
        let mut writer = writer;
        let Some(exit) = exit_id(exit) else {
            return link::write_control(
                &mut writer,
                &Control::Failed("no device is named to route through".to_owned()),
            )
            .await;
        };
        let tunnel = match client::connect(&self.endpoint, reach(exit, at)).await {
            Ok(tunnel) => tunnel,
            Err(error) => return link::write_control(&mut writer, &refusal(&error)).await,
        };
        link::write_control(
            &mut writer,
            &Control::Offer {
                offer: tunnel.offer.clone(),
                mtu: tunnel.mtu(),
                carrier: carrier(&self.endpoint).await,
            },
        )
        .await?;
        let pipe = Arc::new(Pipe {
            reader: tokio::sync::Mutex::new(reader),
            writer: tokio::sync::Mutex::new(writer),
        });
        let ended = client::run(&tunnel, Arc::clone(&pipe)).await;
        tracing::info!(?ended, "a route through another device ended");
        pipe.say(&Control::Ended(ended)).await
    }

    /// Take this link as this device's exit TUN until it ends: a person's other devices route
    /// through it while it is lent.
    async fn lend<R, W>(&self, mut reader: R, writer: W) -> io::Result<()>
    where
        R: tokio::io::AsyncRead + Unpin + Send + Sync + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + Sync + 'static,
    {
        let writer = Arc::new(tokio::sync::Mutex::new(writer));
        let pump = Arc::new(LinkPump::new(
            Arc::clone(&writer) as Arc<tokio::sync::Mutex<dyn WritePacket>>
        ));
        let taken = {
            let mut lent = self.lent.lock_recover();
            if lent.is_some() {
                false
            } else {
                *lent = Some(Arc::clone(&pump));
                true
            }
        };
        if !taken {
            return link::write_control(
                &mut *writer.lock().await,
                &Control::Failed("this device's exit is lent already".to_owned()),
            )
            .await;
        }
        link::write_control(&mut *writer.lock().await, &Control::Ready).await?;
        self.allowed.on(true);
        // Who routes through this device, as the shell's status shows it, until the link ends.
        let following = {
            let (exit, writer) = (self.exit.clone(), Arc::clone(&writer));
            tokio::spawn(async move {
                let mut last: Option<Vec<String>> = None;
                loop {
                    let users = users(&exit);
                    if last.as_ref() != Some(&users) {
                        let said = Control::Users {
                            devices: users.clone(),
                        };
                        if link::write_control(&mut *writer.lock().await, &said)
                            .await
                            .is_err()
                        {
                            return;
                        }
                        last = Some(users);
                    }
                    tokio::time::sleep(USERS_EVERY).await;
                }
            })
        };
        // What the shell asks while it lends the TUN: stop one of the devices routing through it.
        let read = async {
            loop {
                match link::read(&mut reader).await {
                    Ok(Frame::Packet(packet)) => pump.heard(packet),
                    Ok(Frame::Control(Control::Stop { device })) => {
                        if let Some(peer) = exit_id(&device) {
                            self.exit.stop(&peer);
                        }
                    }
                    Ok(Frame::Control(other)) => {
                        tracing::debug!(?other, "the shell said what an exit link does not take");
                    }
                    Err(_) => return,
                }
            }
        };
        read.await;
        following.abort();
        self.allowed.on(false);
        self.lent.lock_recover().take();
        self.exit.recheck();
        Ok(())
    }
}

/// The devices routing through this device, by their endpoint id in hex.
fn users(exit: &Exit<Allowed, LentExit>) -> Vec<String> {
    exit.sessions()
        .into_iter()
        .map(|session| hex::encode(session.peer.as_bytes()))
        .collect()
}

/// What the daemon answers a dial that failed: the exit's refusal as the close said it, or why it
/// was not reached.
fn refusal(error: &client::ClientError) -> Control {
    match error {
        client::ClientError::Refused(refusal) => Control::Refused(*refusal),
        client::ClientError::Unreachable(reason) => Control::Failed(reason.clone()),
    }
}

/// Where `exit` is reached: the address the shell knew, else what this device's endpoint finds by
/// itself.
fn reach(exit: EndpointId, at: Option<EndpointAddr>) -> EndpointAddr {
    let mut addr = EndpointAddr::new(exit);
    if let Some(at) = at {
        for ip in at.ip_addrs() {
            addr = addr.with_ip_addr(*ip);
        }
        for url in at.relay_urls() {
            addr = addr.with_relay_url(url.clone());
        }
    }
    addr
}

/// The device `exit` names, by the endpoint id in hex a shell writes.
fn exit_id(hex_id: &str) -> Option<EndpointId> {
    let mut key = [0_u8; 32];
    hex::decode_to_slice(hex_id.trim(), &mut key).ok()?;
    EndpointId::from_bytes(&key).ok()
}

/// What must reach the exit outside the tunnel: this endpoint's sockets and the relays.
async fn carrier(endpoint: &Endpoint) -> Carrier {
    let mut udp_ports: Vec<u16> = endpoint
        .bound_sockets()
        .iter()
        .map(SocketAddr::port)
        .filter(|port| *port != 0)
        .collect();
    udp_ports.sort_unstable();
    udp_ports.dedup();
    udp_ports.truncate(MAX_CARRIER_PORTS);
    Carrier {
        udp_ports,
        relays: relay_addresses().await,
    }
}

/// The relays' addresses, kept from the last lookup that found any: a lookup while the kill
/// switch blocks finds none.
static RELAYS: Mutex<Vec<IpAddr>> = Mutex::new(Vec::new());

async fn relay_addresses() -> Vec<IpAddr> {
    let urls: Vec<iroh::RelayUrl> = tilo_endpoint_reachability::relays().relays.urls();
    let mut found = tokio::time::timeout(RELAY_LOOKUP, async move {
        let mut found = Vec::new();
        for url in urls {
            let Some(host) = url.host_str() else {
                continue;
            };
            if let Ok(addresses) = tokio::net::lookup_host((host, 443)).await {
                found.extend(
                    addresses
                        .map(|address| address.ip())
                        .filter(|ip| is_relay_address(*ip)),
                );
            }
        }
        found
    })
    .await
    .unwrap_or_default();
    found.sort_unstable();
    found.dedup();
    found.truncate(MAX_RELAYS);
    let mut kept = RELAYS.lock_recover();
    if !found.is_empty() {
        kept.clone_from(&found);
    }
    kept.clone()
}

/// Who may route through this device: one of the person's devices, less the ones they took back,
/// and only while the person turned this device's exit on by lending its TUN.
#[derive(Clone)]
struct Allowed(Arc<Who>);

struct Who {
    /// The person's devices, as the person's own workspace lists them.
    devices: Devices,
    on: AtomicBool,
}

impl Allowed {
    fn on(&self, on: bool) {
        self.0.on.store(on, Ordering::SeqCst);
    }
}

impl Authority for Allowed {
    fn decide(&self, peer: &EndpointId) -> Decision {
        decide(
            &(self.0.devices)(),
            self.0.on.load(Ordering::SeqCst),
            peer.as_bytes(),
        )
    }

    fn home_networks(&self) -> Vec<Ipv4Net> {
        home_networks()
    }
}

/// Whether `peer` may route through this device: one of the person's devices (`allowed`), and
/// only while its exit is on (`on`).
fn decide(allowed: &BTreeSet<[u8; 32]>, on: bool, peer: &[u8; 32]) -> Decision {
    if !allowed.contains(peer) {
        Decision::NotYours
    } else if !on {
        Decision::ExitOff
    } else {
        Decision::Allow
    }
}

/// The networks this device is attached to, other than the tunnel's own.
fn home_networks() -> Vec<Ipv4Net> {
    ipv4_subnets()
        .into_iter()
        .filter(|(name, _)| {
            name != device_vpn::net::CLIENT_INTERFACE && name != device_vpn::net::EXIT_INTERFACE
        })
        .filter_map(|(_, subnet)| Ipv4Net::new(Ipv4Addr::from(subnet.network), subnet.prefix))
        .collect()
}

/// This device's exit TUN, lent by a shell: the exit opens it when a device of the person's
/// routes through this one, and keeps it while the shell lends it.
struct LentExit(Arc<Mutex<Option<Arc<LinkPump>>>>);

/// One exit's use of the lent TUN.
#[derive(Clone)]
struct Lent(Arc<LinkPump>);

impl PacketDevice for Lent {
    async fn recv<'a>(&'a self, buf: &'a mut [u8]) -> io::Result<usize> {
        self.0.recv(buf).await
    }

    async fn send<'a>(&'a self, packet: &'a [u8]) -> io::Result<()> {
        self.0.send(packet).await
    }
}

impl ExitDevices for LentExit {
    type Device = Lent;

    async fn open(&self, _mtu: u16) -> io::Result<Lent> {
        self.0
            .lock_recover()
            .clone()
            .map(Lent)
            .ok_or_else(|| io::Error::other("this device's exit TUN is not lent"))
    }

    /// The shell keeps its TUN while the person's exit is on, so the exit closing its own use of
    /// it lends the same one again when a device comes back.
    async fn close(&self) {}
}

/// One open tunnel's packets over its link, both ways.
struct Pipe<R, W> {
    reader: tokio::sync::Mutex<R>,
    writer: tokio::sync::Mutex<W>,
}

impl<R, W> Pipe<R, W>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    async fn say(&self, control: &Control) -> io::Result<()> {
        let mut writer = self.writer.lock().await;
        link::write_control(&mut *writer, control).await?;
        tokio::io::AsyncWriteExt::shutdown(&mut *writer).await
    }
}

impl<R, W> PacketDevice for Pipe<R, W>
where
    R: tokio::io::AsyncRead + Unpin + Send + Sync + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + Sync + 'static,
{
    async fn recv<'a>(&'a self, buf: &'a mut [u8]) -> io::Result<usize> {
        let mut reader = self.reader.lock().await;
        loop {
            match link::read(&mut *reader).await? {
                Frame::Packet(packet) => {
                    let length = packet.len().min(buf.len());
                    if let (Some(into), Some(from)) = (buf.get_mut(..length), packet.get(..length))
                    {
                        into.copy_from_slice(from);
                    }
                    return Ok(length);
                }
                Frame::Control(control) => {
                    tracing::debug!(?control, "a route's link said what it does not take");
                }
            }
        }
    }

    async fn send<'a>(&'a self, packet: &'a [u8]) -> io::Result<()> {
        let mut writer = self.writer.lock().await;
        link::write_packet(&mut *writer, packet).await
    }
}

/// A lent exit TUN's packets over its link: what the exit's read loop takes, fed by the link's
/// reader, and what the exit sends, written by the link's writer.
struct LinkPump {
    packets: Mutex<VecDeque<Vec<u8>>>,
    arrived: tokio::sync::Notify,
    writer: Arc<tokio::sync::Mutex<dyn WritePacket>>,
}

/// The writing half of a lent exit link.
trait WritePacket: Send + Sync {
    fn write<'a>(
        &'a mut self,
        packet: &'a [u8],
    ) -> std::pin::Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>>;
}

impl<W> WritePacket for W
where
    W: tokio::io::AsyncWrite + Unpin + Send + Sync,
{
    fn write<'a>(
        &'a mut self,
        packet: &'a [u8],
    ) -> std::pin::Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>> {
        Box::pin(link::write_packet(self, packet))
    }
}

impl LinkPump {
    fn new(writer: Arc<tokio::sync::Mutex<dyn WritePacket>>) -> Self {
        Self {
            packets: Mutex::new(VecDeque::new()),
            arrived: tokio::sync::Notify::new(),
            writer,
        }
    }

    /// One packet the shell's TUN read, kept for the exit's next read.
    fn heard(&self, packet: Vec<u8>) {
        let mut packets = self.packets.lock_recover();
        if packets.len() >= PACKETS_KEPT {
            packets.pop_front();
        }
        packets.push_back(packet);
        drop(packets);
        self.arrived.notify_one();
    }
}

impl PacketDevice for LinkPump {
    async fn recv<'a>(&'a self, buf: &'a mut [u8]) -> io::Result<usize> {
        loop {
            {
                let mut packets = self.packets.lock_recover();
                if let Some(packet) = packets.pop_front() {
                    let length = packet.len().min(buf.len());
                    if let (Some(into), Some(from)) = (buf.get_mut(..length), packet.get(..length))
                    {
                        into.copy_from_slice(from);
                    }
                    return Ok(length);
                }
            }
            self.arrived.notified().await;
        }
    }

    async fn send<'a>(&'a self, packet: &'a [u8]) -> io::Result<()> {
        let mut writer = self.writer.lock().await;
        writer.write(packet).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the person's devices are allowed, and only while the exit is on.
    #[test]
    fn only_the_persons_devices_route_through_this_one_while_its_exit_is_on() {
        let theirs = *iroh::SecretKey::generate().public().as_bytes();
        let other = *iroh::SecretKey::generate().public().as_bytes();
        let allowed: BTreeSet<[u8; 32]> = [theirs].into_iter().collect();
        assert_eq!(
            decide(&allowed, true, &theirs),
            Decision::Allow,
            "one of the person's devices, while the exit is on"
        );
        assert_eq!(
            decide(&allowed, false, &theirs),
            Decision::ExitOff,
            "the person turned the exit off"
        );
        assert_eq!(
            decide(&allowed, true, &other),
            Decision::NotYours,
            "a device that is not theirs, whatever the switch says"
        );
        // The person's record is not here, so no device of theirs is known: this device turns
        // everyone away rather than trusting a device it cannot check.
        let devices: Devices = Arc::new(BTreeSet::new);
        let allowed = Allowed(Arc::new(Who {
            devices,
            on: AtomicBool::new(true),
        }));
        let peer = EndpointId::from_bytes(&theirs).unwrap();
        assert_eq!(allowed.decide(&peer), Decision::NotYours);
        assert_eq!(allowed.home_networks(), home_networks());
        const {
            assert!(device_vpn::net::MAX_MTU >= device_vpn::net::MIN_MTU);
        }
    }
}
