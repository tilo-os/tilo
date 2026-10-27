//! Wi-Fi Direct group formation through NetworkManager.
//!
//! NetworkManager owns the Wi-Fi Direct device: it finds peers, negotiates
//! the group (with the WFD information elements this source announces) and
//! configures its IP address. The group is bound to the D-Bus connection
//! that asked for it, so it comes down if the process holding it dies.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::ie;

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIFI_P2P: &str = "org.freedesktop.NetworkManager.Device.WifiP2P";
const PEER: &str = "org.freedesktop.NetworkManager.WifiP2PPeer";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const IP4_CONFIG: &str = "org.freedesktop.NetworkManager.IP4Config";
const DEVICE_TYPE_WIFI_P2P: u32 = 30;
const ACTIVATED: u32 = 2;
const DEACTIVATED: u32 = 4;
const POLL: Duration = Duration::from_millis(250);

/// Why a group could not be formed.
#[derive(Debug)]
#[non_exhaustive]
pub enum P2pError {
    /// NetworkManager could not be reached or refused a call.
    Bus(zbus::Error),
    /// This system has no Wi-Fi Direct device.
    NoDevice,
    /// The peer did not show up in a scan.
    PeerNotFound(String),
    /// The group did not come up.
    Activation(String),
}

impl fmt::Display for P2pError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(error) => write!(f, "NetworkManager: {error}"),
            Self::NoDevice => f.write_str("this device has no Wi-Fi Direct support"),
            Self::PeerNotFound(peer) => write!(f, "the receiver {peer} is not in range"),
            Self::Activation(detail) => write!(f, "the Wi-Fi Direct connection failed: {detail}"),
        }
    }
}

impl std::error::Error for P2pError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bus(error) => Some(error),
            _ => None,
        }
    }
}

impl From<zbus::Error> for P2pError {
    fn from(error: zbus::Error) -> Self {
        Self::Bus(error)
    }
}

impl From<zbus::fdo::Error> for P2pError {
    fn from(error: zbus::fdo::Error) -> Self {
        Self::Bus(error.into())
    }
}

/// How to form a group.
#[derive(Clone, Debug)]
pub struct ConnectOptions {
    /// The RTSP port this source listens on, announced in its WFD IEs.
    pub rtsp_port: u16,
    /// How long to scan for the peer.
    pub find_timeout: Duration,
    /// How long group negotiation and addressing may take.
    pub activation_timeout: Duration,
    /// The name of the (volatile) connection profile.
    pub connection_name: String,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            rtsp_port: ie::DEFAULT_RTSP_PORT,
            find_timeout: Duration::from_secs(15),
            activation_timeout: Duration::from_secs(40),
            connection_name: "Wi-Fi Display".to_owned(),
        }
    }
}

/// A formed Wi-Fi Direct group with a sink.
#[derive(Debug)]
pub struct P2pGroup {
    connection: zbus::Connection,
    active: OwnedObjectPath,
    /// The group's network interface, such as `p2p-wlp1s0-0`.
    pub interface: String,
    /// This device's address in the group.
    pub local: Ipv4Addr,
    /// The group's prefix length.
    pub prefix: u8,
    /// The gateway NetworkManager learned: the sink, when it owns the group.
    pub gateway: Option<Ipv4Addr>,
    /// The sink's RTSP port from its WFD IEs.
    pub sink_rtsp_port: u16,
    /// The sink's name as it announced itself.
    pub sink_name: String,
}

impl P2pGroup {
    /// Whether `address` is in the group's subnet.
    #[must_use]
    pub fn contains(&self, address: IpAddr) -> bool {
        let IpAddr::V4(address) = address else {
            return false;
        };
        let mask = u32::MAX
            .checked_shl(32 - u32::from(self.prefix))
            .unwrap_or(0);
        u32::from(address) & mask == u32::from(self.local) & mask
    }

    /// The sink's address: the gateway, or the group interface's neighbour.
    #[must_use]
    pub fn sink_address(&self) -> Option<Ipv4Addr> {
        self.gateway.or_else(|| {
            let table = std::fs::read_to_string("/proc/net/arp").ok()?;
            neighbour_on(&table, &self.interface).filter(|address| *address != self.local)
        })
    }

    /// Takes the group down.
    ///
    /// # Errors
    ///
    /// NetworkManager refused.
    pub async fn close(self) -> Result<(), P2pError> {
        let manager = zbus::Proxy::new(&self.connection, NM, NM_PATH, NM).await?;
        manager
            .call_method("DeactivateConnection", &(&self.active,))
            .await?;
        Ok(())
    }
}

/// The first complete entry for `interface` in `/proc/net/arp`'s text.
fn neighbour_on(table: &str, interface: &str) -> Option<Ipv4Addr> {
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [address, _, flags, _, _, device] = fields.as_slice() else {
            return None;
        };
        let complete =
            u32::from_str_radix(flags.trim_start_matches("0x"), 16).unwrap_or(0) & 0x2 != 0;
        (complete && *device == interface)
            .then(|| address.parse().ok())
            .flatten()
    })
}

async fn proxy<'a>(
    connection: &zbus::Connection,
    path: &'a str,
    interface: &'a str,
) -> Result<zbus::Proxy<'a>, P2pError> {
    Ok(zbus::Proxy::new(connection, NM, path, interface).await?)
}

async fn p2p_device(connection: &zbus::Connection) -> Result<OwnedObjectPath, P2pError> {
    let manager = proxy(connection, NM_PATH, NM).await?;
    let reply = manager.call_method("GetDevices", &()).await?;
    let devices: Vec<OwnedObjectPath> = reply.body().deserialize()?;
    for path in devices {
        let device = proxy(connection, path.as_str(), DEVICE).await?;
        if device.get_property::<u32>("DeviceType").await? == DEVICE_TYPE_WIFI_P2P {
            return Ok(path);
        }
    }
    Err(P2pError::NoDevice)
}

struct FoundPeer {
    path: OwnedObjectPath,
    name: String,
    rtsp_port: u16,
}

async fn find_peer(
    connection: &zbus::Connection,
    device: &OwnedObjectPath,
    mac: &str,
    timeout: Duration,
) -> Result<FoundPeer, P2pError> {
    let p2p = proxy(connection, device.as_str(), WIFI_P2P).await?;
    let seconds = i32::try_from(timeout.as_secs().clamp(1, 120)).unwrap_or(30);
    let options = HashMap::from([("timeout", Value::from(seconds))]);
    p2p.call_method("StartFind", &(options,)).await?;
    let deadline = Instant::now() + timeout;
    let found = loop {
        let peers: Vec<OwnedObjectPath> = p2p.get_property("Peers").await?;
        let mut hit = None;
        for path in peers {
            let peer = proxy(connection, path.as_str(), PEER).await?;
            let address: String = peer.get_property("HwAddress").await?;
            if address.eq_ignore_ascii_case(mac) {
                let ies: Vec<u8> = peer.get_property("WfdIEs").await.unwrap_or_default();
                let name: String = peer.get_property("Name").await.unwrap_or_default();
                let rtsp_port = ie::device_info(&ies)
                    .map(|info| info.rtsp_port)
                    .filter(|port| *port != 0)
                    .unwrap_or(ie::DEFAULT_RTSP_PORT);
                hit = Some(FoundPeer {
                    path,
                    name,
                    rtsp_port,
                });
                break;
            }
        }
        if let Some(found) = hit {
            break Ok(found);
        }
        if Instant::now() >= deadline {
            break Err(P2pError::PeerNotFound(mac.to_owned()));
        }
        tokio::time::sleep(POLL).await;
    };
    let _ = p2p.call_method("StopFind", &()).await;
    found
}

/// Forms a Wi-Fi Direct group with the sink whose P2P device address is
/// `mac`, announcing this source's WFD IEs, and waits until the group has
/// an IP address.
///
/// # Errors
///
/// No Wi-Fi Direct device, the peer is out of range, or NetworkManager
/// could not form the group in time.
pub async fn connect(
    connection: &zbus::Connection,
    mac: &str,
    options: &ConnectOptions,
) -> Result<P2pGroup, P2pError> {
    let device = p2p_device(connection).await?;
    let peer = find_peer(connection, &device, mac, options.find_timeout).await?;
    let settings: HashMap<&str, HashMap<&str, Value>> = HashMap::from([
        (
            "connection",
            HashMap::from([
                ("id", Value::from(options.connection_name.as_str())),
                ("type", Value::from("wifi-p2p")),
                ("autoconnect", Value::from(false)),
            ]),
        ),
        (
            "wifi-p2p",
            HashMap::from([
                ("peer", Value::from(mac)),
                ("wfd-ies", Value::from(ie::source_ies(options.rtsp_port))),
            ]),
        ),
        (
            "ipv4",
            HashMap::from([
                ("method", Value::from("auto")),
                ("never-default", Value::from(true)),
            ]),
        ),
        ("ipv6", HashMap::from([("method", Value::from("ignore"))])),
    ]);
    // Volatile: no profile outlives the session. Bound to this D-Bus
    // client: the group ends with the process that asked for it.
    let activation: HashMap<&str, Value> = HashMap::from([
        ("persist", Value::from("volatile")),
        ("bind-activation", Value::from("dbus-client")),
    ]);
    let manager = proxy(connection, NM_PATH, NM).await?;
    let reply = manager
        .call_method(
            "AddAndActivateConnection2",
            &(settings, &device, &peer.path, activation),
        )
        .await?;
    let (_profile, active, _result): (
        OwnedObjectPath,
        OwnedObjectPath,
        HashMap<String, OwnedValue>,
    ) = reply.body().deserialize()?;
    match wait_for_group(connection, &active, options.activation_timeout).await {
        Ok((interface, local, prefix, gateway)) => Ok(P2pGroup {
            connection: connection.clone(),
            active,
            interface,
            local,
            prefix,
            gateway,
            sink_rtsp_port: peer.rtsp_port,
            sink_name: peer.name,
        }),
        Err(failure) => {
            let _ = manager
                .call_method("DeactivateConnection", &(&active,))
                .await;
            Err(failure)
        }
    }
}

async fn wait_for_group(
    connection: &zbus::Connection,
    active: &OwnedObjectPath,
    timeout: Duration,
) -> Result<(String, Ipv4Addr, u8, Option<Ipv4Addr>), P2pError> {
    let deadline = Instant::now() + timeout;
    let connection_active = proxy(connection, active.as_str(), ACTIVE).await?;
    loop {
        let state: u32 = connection_active.get_property("State").await?;
        if state == ACTIVATED {
            break;
        }
        if state == DEACTIVATED {
            return Err(P2pError::Activation(
                "the receiver declined or the negotiation failed".to_owned(),
            ));
        }
        if Instant::now() >= deadline {
            return Err(P2pError::Activation(format!(
                "not connected within {}s",
                timeout.as_secs()
            )));
        }
        tokio::time::sleep(POLL).await;
    }
    let devices: Vec<OwnedObjectPath> = connection_active.get_property("Devices").await?;
    let device_path = devices
        .first()
        .ok_or_else(|| P2pError::Activation("the group has no device".to_owned()))?;
    let device = proxy(connection, device_path.as_str(), DEVICE).await?;
    let interface: String = device.get_property("IpInterface").await?;
    let config: OwnedObjectPath = device.get_property("Ip4Config").await?;
    let ip4 = proxy(connection, config.as_str(), IP4_CONFIG).await?;
    let addresses: Vec<HashMap<String, OwnedValue>> = ip4.get_property("AddressData").await?;
    let (local, prefix) = addresses
        .iter()
        .find_map(|entry| {
            let address = String::try_from(entry.get("address")?.try_clone().ok()?).ok()?;
            let prefix = u32::try_from(entry.get("prefix")?.try_clone().ok()?).ok()?;
            Some((
                address.parse::<Ipv4Addr>().ok()?,
                u8::try_from(prefix).ok()?,
            ))
        })
        .ok_or_else(|| P2pError::Activation("the group has no IPv4 address".to_owned()))?;
    let gateway: String = ip4.get_property("Gateway").await.unwrap_or_default();
    Ok((interface, local, prefix, gateway.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_neighbour_on_the_group_interface_is_the_sink() {
        let table = "IP address       HW type     Flags       HW address            Mask     Device\n\
                     192.168.1.1      0x1         0x2         aa:bb:cc:dd:ee:ff     *        wlp1s0\n\
                     192.168.49.9     0x1         0x0         00:00:00:00:00:00     *        p2p-wlp1s0-0\n\
                     192.168.49.1     0x1         0x2         02:00:00:00:00:09     *        p2p-wlp1s0-0\n";
        assert_eq!(
            neighbour_on(table, "p2p-wlp1s0-0"),
            Some(Ipv4Addr::new(192, 168, 49, 1))
        );
        assert_eq!(neighbour_on(table, "p2p-wlp1s0-1"), None);
    }
}
