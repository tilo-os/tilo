//! The parts of a saved network Settings shows on a network's own page and
//! a VPN's: how the link in use stands (band, speed, addresses), the
//! profile's IPv6, proxy and 802.1X identity, its password, and a VPN
//! profile's tunnel (endpoint, routes, MTU, keepalive), all through
//! `NetworkManager`'s D-Bus client [`tilo_nm`]. Secrets travel only in the
//! D-Bus calls that carry them.

use std::collections::HashMap;
use std::fmt::Write as _;

use base64::Engine as _;
use tilo_nm::{DeviceKind, IpFamily, NmError, Settings, WireGuardPeer};
use zeroize::Zeroizing;

use crate::network::{valid_ip_token, validate_uuid, with_nm};

// ---------------------------------------------------------------------------
// IPv6
// ---------------------------------------------------------------------------

/// How a connection gets its IPv6 address.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ipv6Method {
    /// From the network (router advertisements, `DHCPv6`).
    #[default]
    Automatic,
    /// The addresses given.
    Manual,
    /// Only the link-local address: no routed IPv6.
    LinkLocal,
    /// No IPv6 at all.
    Disabled,
}

impl Ipv6Method {
    /// `NetworkManager`'s name for it (`ipv6.method`).
    const fn method(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Manual => "manual",
            Self::LinkLocal => "link-local",
            Self::Disabled => "disabled",
        }
    }

    fn of_method(value: &str) -> Option<Self> {
        Some(match value {
            "auto" | "dhcp" => Self::Automatic,
            "manual" => Self::Manual,
            "link-local" => Self::LinkLocal,
            "disabled" | "ignore" => Self::Disabled,
            _ => return None,
        })
    }
}

/// A profile's IPv6 settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ipv6Configuration {
    pub method: Ipv6Method,
    /// Addresses with their prefix (`2001:db8::10/64`), for a manual method.
    pub addresses: Vec<String>,
    pub gateway: Option<String>,
    /// Name servers of its own; empty takes the network's.
    pub dns: Vec<String>,
}

/// A profile's IPv6 settings, `None` when its method is one this does not
/// edit (shared) or a value is unreadable.
pub(crate) fn ipv6_of(settings: &Settings) -> Option<Ipv6Configuration> {
    let family = IpFamily::V6;
    // `NetworkManager`'s default when the profile leaves it out.
    let method = Ipv6Method::of_method(settings.method(family).as_deref().unwrap_or("auto"))?;
    let addresses = settings.addresses(family);
    let dns = settings.dns(family);
    if addresses.len() > 16
        || dns.len() > 16
        || addresses.iter().chain(&dns).any(|v| !valid_ip_token(v))
    {
        return None;
    }
    Some(Ipv6Configuration {
        method,
        addresses,
        gateway: settings.gateway(family),
        dns,
    })
}

/// Apply one profile's IPv6 settings.
///
/// # Errors
/// An invalid id, address, gateway or name server, a manual method without
/// an address, or `NetworkManager` refusing.
pub fn configure_ipv6(uuid: &str, config: &Ipv6Configuration) -> Result<(), String> {
    validate_uuid(uuid)?;
    if config.addresses.len() > 16
        || config.dns.len() > 16
        || config.addresses.iter().any(|v| !valid_ip_token(v))
        || config
            .gateway
            .as_deref()
            .is_some_and(|v| !valid_ip_token(v))
        || config.dns.iter().any(|v| !valid_ip_token(v))
    {
        return Err("invalid IPv6 address, prefix, gateway, or DNS server".into());
    }
    if config.method == Ipv6Method::Manual && config.addresses.is_empty() {
        return Err("manual IPv6 needs at least one address with a prefix".into());
    }
    let config = config.clone();
    with_nm(|nm| async move {
        nm.edit(uuid, |settings| {
            let family = IpFamily::V6;
            let manual = config.method == Ipv6Method::Manual;
            settings.set(family.setting(), "method", config.method.method());
            let addresses = if manual {
                config.addresses.as_slice()
            } else {
                &[]
            };
            settings.set_addresses(family, addresses)?;
            settings.set_gateway(family, config.gateway.as_deref().filter(|_| manual))?;
            settings.set_dns(family, &config.dns)?;
            settings.set(family.setting(), "ignore-auto-dns", !config.dns.is_empty());
            Ok(())
        })
        .await
    })
}

/// Give a profile name servers of its own, or (empty) take the network's.
/// IPv4 and IPv6 servers go to their own family.
///
/// # Errors
/// An invalid id or server, or `NetworkManager` refusing.
pub fn configure_dns(uuid: &str, servers: &[String]) -> Result<(), String> {
    validate_uuid(uuid)?;
    if servers.len() > 16 || servers.iter().any(|server| !valid_ip_token(server)) {
        return Err("invalid DNS server".into());
    }
    let (v6, v4): (Vec<String>, Vec<String>) = servers
        .iter()
        .cloned()
        .partition(|server| server.contains(':'));
    with_nm(|nm| async move {
        nm.edit(uuid, |settings| {
            for (family, list) in [(IpFamily::V4, &v4), (IpFamily::V6, &v6)] {
                settings.set_dns(family, list)?;
                settings.set(family.setting(), "ignore-auto-dns", !list.is_empty());
            }
            Ok(())
        })
        .await
    })
}

/// The text of the QR code a phone scans to join a Wi-Fi network: the
/// `WIFI:` scheme Android and iOS read. `password` is `None` for an open
/// network.
#[must_use]
pub fn wifi_qr_payload(ssid: &str, password: Option<&str>, hidden: bool) -> Zeroizing<String> {
    let escape = |value: &str| {
        value
            .replace('\\', "\\\\")
            .replace(';', "\\;")
            .replace(',', "\\,")
            .replace(':', "\\:")
            .replace('"', "\\\"")
    };
    let mut text = Zeroizing::new(String::from("WIFI:"));
    if let Some(password) = password {
        text.push_str("T:WPA;S:");
        text.push_str(&escape(ssid));
        text.push_str(";P:");
        text.push_str(&Zeroizing::new(escape(password)));
        text.push(';');
    } else {
        text.push_str("T:nopass;S:");
        text.push_str(&escape(ssid));
        text.push(';');
    }
    if hidden {
        text.push_str("H:true;");
    }
    text.push(';');
    text
}

// ---------------------------------------------------------------------------
// Work or school (802.1X) identity
// ---------------------------------------------------------------------------

/// Who a work or school network knows this device as, and the certificate
/// it checks the network with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnterpriseIdentity {
    pub identity: String,
    /// The CA certificate's path; empty when the network is not checked.
    pub ca_certificate: String,
}

pub(crate) fn enterprise_identity_of(settings: &Settings) -> Option<EnterpriseIdentity> {
    let identity = settings
        .string("802-1x", "identity")
        .filter(|identity| !identity.is_empty())?;
    Some(EnterpriseIdentity {
        identity,
        ca_certificate: settings.path("802-1x", "ca-cert").unwrap_or_default(),
    })
}

/// Change a work or school network's identity and CA certificate.
///
/// # Errors
/// An invalid id, an identity or path with a line break, a relative path,
/// or `NetworkManager` refusing.
pub fn configure_enterprise_identity(
    uuid: &str,
    identity: &EnterpriseIdentity,
) -> Result<(), String> {
    validate_uuid(uuid)?;
    let bad = |value: &str| value.len() > 1024 || value.chars().any(char::is_control);
    if identity.identity.trim().is_empty()
        || bad(&identity.identity)
        || bad(&identity.ca_certificate)
    {
        return Err("the identity or certificate path is invalid".into());
    }
    if !identity.ca_certificate.is_empty() && !identity.ca_certificate.starts_with('/') {
        return Err("the CA certificate is a full path".into());
    }
    let identity = identity.clone();
    with_nm(|nm| async move {
        nm.edit(uuid, |settings| {
            settings.set("802-1x", "identity", identity.identity.as_str());
            settings.set_path("802-1x", "ca-cert", &identity.ca_certificate);
            Ok(())
        })
        .await
    })
}

// ---------------------------------------------------------------------------
// One proxy server, kept as an auto-configuration script
// ---------------------------------------------------------------------------

const PROXY_SCRIPT_START: &str = "function FindProxyForURL(url, host) { return \"PROXY ";
const PROXY_SCRIPT_END: &str = "\"; }";

/// Whether `server` is `host:port`: a host name or address of the
/// characters a host can have, and a port.
#[must_use]
pub fn valid_proxy_server(server: &str) -> bool {
    let Some((host, port)) = server.rsplit_once(':') else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    !host.is_empty()
        && server.len() <= 255
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_'))
        && port.parse::<u16>().is_ok_and(|port| port > 0)
}

/// The one-line auto-configuration script that sends everything through
/// `server`.
#[must_use]
pub fn manual_proxy_script(server: &str) -> String {
    format!("{PROXY_SCRIPT_START}{server}{PROXY_SCRIPT_END}")
}

/// The server a script [`manual_proxy_script`] wrote names; `None` for any
/// other script.
#[must_use]
pub fn manual_proxy_of(script: &str) -> Option<String> {
    let server = script
        .trim()
        .strip_prefix(PROXY_SCRIPT_START)?
        .strip_suffix(PROXY_SCRIPT_END)?;
    valid_proxy_server(server).then(|| server.to_string())
}

// ---------------------------------------------------------------------------
// The Wi-Fi link in use
// ---------------------------------------------------------------------------

/// How the Wi-Fi link in use stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiLinkDetails {
    /// The device it is on (`wlan0`).
    pub device: String,
    pub frequency_mhz: Option<u32>,
    /// The link's bit rate as `NetworkManager` says it (`866 Mbit/s`).
    pub rate: Option<String>,
    /// The hardware address in use, which a random-address policy changes.
    pub hardware_address: Option<String>,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
}

/// Which band a frequency is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiBand {
    TwoPointFour,
    Five,
    Six,
}

impl WifiBand {
    /// The band `mhz` is in, if it is a Wi-Fi band.
    #[must_use]
    pub fn of_frequency(mhz: u32) -> Option<Self> {
        match mhz {
            2400..=2500 => Some(Self::TwoPointFour),
            4900..=5900 => Some(Self::Five),
            5925..=7125 => Some(Self::Six),
            _ => None,
        }
    }
}

/// The link the connected Wi-Fi device is on.
///
/// # Errors
/// No Wi-Fi device is connected, or `NetworkManager` cannot be asked.
pub fn wifi_link_details() -> Result<WifiLinkDetails, String> {
    with_nm(|nm| async move {
        let device = nm
            .devices()
            .await?
            .into_iter()
            .find(|device| device.kind == DeviceKind::Wifi && device.is_activated())
            .ok_or_else(|| NmError::NotFound("no Wi-Fi device is connected".into()))?;
        let v4 = nm.ip_details(&device.path, IpFamily::V4).await?;
        let v6 = nm
            .ip_details(&device.path, IpFamily::V6)
            .await
            .unwrap_or_default();
        let point = nm
            .access_points()
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|point| point.device == device.path && point.active);
        Ok(WifiLinkDetails {
            frequency_mhz: point
                .as_ref()
                .map(|point| point.frequency)
                .filter(|mhz| *mhz > 0),
            rate: point
                .as_ref()
                .map(|point| point.max_bitrate / 1000)
                .filter(|rate| *rate > 0)
                .map(|rate| format!("{rate} Mbit/s")),
            hardware_address: Some(device.hw_address).filter(|address| !address.is_empty()),
            device: device.interface,
            ipv4: v4.addresses,
            ipv6: v6.addresses,
            gateway: v4.gateway,
            dns: v4.dns.into_iter().chain(v6.dns).collect(),
        })
    })
}

// ---------------------------------------------------------------------------
// Passwords and hidden networks
// ---------------------------------------------------------------------------

/// A saved Wi-Fi network's password. Reading it asks `NetworkManager`, which
/// asks the person to authenticate when the profile is the system's.
///
/// # Errors
/// An invalid id, a network without a password, or the request refused.
pub fn wifi_profile_password(uuid: &str) -> Result<Zeroizing<String>, String> {
    validate_uuid(uuid)?;
    let password = with_nm(|nm| async move {
        let profile = nm.profile(uuid).await?;
        let secrets = nm.secrets(&profile, "802-11-wireless-security").await?;
        Ok(Zeroizing::new(
            secrets
                .string("802-11-wireless-security", "psk")
                .unwrap_or_default(),
        ))
    })?;
    if password.is_empty() {
        return Err("this network has no saved password".into());
    }
    Ok(password)
}

/// Change a saved Wi-Fi network's password.
///
/// # Errors
/// An invalid id or password, or `NetworkManager` refusing.
pub fn set_wifi_profile_password(uuid: &str, password: &str) -> Result<(), String> {
    validate_uuid(uuid)?;
    if !(8..=63).contains(&password.len()) || password.chars().any(char::is_control) {
        return Err("a Wi-Fi password is 8 to 63 characters, without line breaks".into());
    }
    let password = Zeroizing::new(password.to_owned());
    with_nm(|nm| async move {
        nm.edit(uuid, |settings| {
            settings.set("802-11-wireless-security", "psk", password.as_str());
            Ok(())
        })
        .await
    })
}

/// Join a network that does not broadcast its name.
///
/// # Errors
/// An invalid name or password, or the network refusing to be joined.
pub fn connect_hidden_wifi(ssid: &str, password: Option<&str>) -> Result<(), String> {
    if ssid.is_empty() || ssid.len() > 32 || ssid.chars().any(char::is_control) {
        return Err("a network name is 1 to 32 characters".into());
    }
    crate::network::join_wifi(ssid, password, true)
}

// ---------------------------------------------------------------------------
// VPN profiles
// ---------------------------------------------------------------------------

/// A VPN protocol `NetworkManager` can run here: `WireGuard` built in, the
/// others as installed plugins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VpnPlugin {
    /// Its service type (`org.freedesktop.NetworkManager.openvpn`).
    pub service: String,
    /// What it is called (`OpenVPN`).
    pub name: String,
}

/// The VPN plugins installed: each a `.name` file `NetworkManager` reads.
#[must_use]
pub fn installed_vpn_plugins() -> Vec<VpnPlugin> {
    let mut plugins = Vec::new();
    for dir in ["/usr/lib/NetworkManager/VPN", "/etc/NetworkManager/VPN"] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("name") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(plugin) = parse_vpn_plugin(&text)
                && !plugins
                    .iter()
                    .any(|known: &VpnPlugin| known.service == plugin.service)
            {
                plugins.push(plugin);
            }
        }
    }
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    plugins
}

fn parse_vpn_plugin(text: &str) -> Option<VpnPlugin> {
    let mut service = None;
    let mut name = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "service" => service = Some(value.trim().to_string()),
            "name" => name = Some(value.trim().to_string()),
            _ => {}
        }
    }
    let service = service.filter(|s| !s.is_empty())?;
    let name = name.filter(|n| !n.is_empty()).map_or_else(
        || service.clone(),
        |name| known_plugin_name(&name).map_or(name, str::to_owned),
    );
    Some(VpnPlugin { service, name })
}

/// What the plugins `NetworkManager` ships are called, by their short name
/// (`openvpn`).
fn known_plugin_name(short: &str) -> Option<&'static str> {
    Some(match short {
        "openvpn" => "OpenVPN",
        "vpnc" => "Cisco Compatible VPN (vpnc)",
        "openconnect" => "OpenConnect",
        "l2tp" => "L2TP",
        "pptp" => "PPTP",
        "strongswan" => "IPsec (strongSwan)",
        "libreswan" => "IPsec (Libreswan)",
        "fortisslvpn" => "Fortinet SSL VPN",
        "sstp" => "SSTP",
        "iodine" => "Iodine",
        _ => return None,
    })
}

/// What the VPN protocol of service type `service`
/// (`org.freedesktop.NetworkManager.openvpn`) is called, whether or not its
/// plugin is installed here.
#[must_use]
pub fn vpn_service_name(service: &str) -> String {
    let short = service.rsplit('.').next().unwrap_or(service);
    known_plugin_name(short).map_or_else(|| short.to_owned(), str::to_owned)
}

/// A plugin VPN profile's service type, which says its protocol.
///
/// # Errors
/// An invalid id, or `NetworkManager` cannot be asked.
pub fn vpn_service_type(uuid: &str) -> Result<Option<String>, String> {
    validate_uuid(uuid)?;
    with_nm(|nm| async move {
        Ok(nm
            .profile(uuid)
            .await?
            .settings
            .string("vpn", "service-type")
            .filter(|service| !service.is_empty()))
    })
}

/// What a VPN profile's page shows and edits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VpnProfileSettings {
    pub name: String,
    /// `wireguard`, or `vpn` for a plugin's profile.
    pub kind: String,
    /// A plugin profile's service type.
    pub service: Option<String>,
    pub autoconnect: bool,
    /// Every connection goes through the tunnel; otherwise only the
    /// networks it routes.
    pub all_traffic: bool,
    pub addresses: Vec<String>,
    pub dns: Vec<String>,
    pub mtu: Option<u32>,
    pub endpoint: Option<String>,
    pub allowed_ips: Vec<String>,
    /// The server's public key (`WireGuard`).
    pub peer_public_key: Option<String>,
    /// Seconds between keepalives (`WireGuard`); `None` sends none.
    pub keepalive: Option<u32>,
}

/// A VPN profile's settings.
///
/// # Errors
/// An invalid id, or `NetworkManager` cannot be asked.
pub fn vpn_profile_settings(uuid: &str) -> Result<VpnProfileSettings, String> {
    validate_uuid(uuid)?;
    with_nm(|nm| async move {
        vpn_settings_of(&nm.profile(uuid).await?.settings).map_err(NmError::Refused)
    })
}

fn vpn_settings_of(profile: &Settings) -> Result<VpnProfileSettings, String> {
    let kind = profile.kind();
    if !tilo_nm::is_vpn_type(&kind) {
        return Err("this profile is not a VPN".into());
    }
    let mut settings = VpnProfileSettings {
        name: profile.id(),
        kind,
        service: profile
            .string("vpn", "service-type")
            .filter(|s| !s.is_empty()),
        autoconnect: profile.autoconnect(),
        all_traffic: !profile.boolean("ipv4", "never-default").unwrap_or(false),
        addresses: profile.addresses(IpFamily::V4),
        dns: profile.dns(IpFamily::V4),
        mtu: profile.uint("wireguard", "mtu").filter(|mtu| *mtu > 0),
        ..VpnProfileSettings::default()
    };
    if let Some(peer) = profile.wireguard_peers().into_iter().next() {
        settings.endpoint = peer.endpoint;
        settings.allowed_ips = peer.allowed_ips;
        settings.peer_public_key = Some(peer.public_key);
        settings.keepalive = peer.keepalive;
    }
    let data = profile.string_map("vpn", "data");
    if let Some(remote) = data.get("remote") {
        settings.endpoint = Some(remote.clone());
    }
    if let Some(mtu) = data.get("tunnel-mtu") {
        settings.mtu = mtu.parse().ok();
    }
    Ok(settings)
}

/// The `vpn.data` keys that name a plugin profile's server: `OpenVPN`'s
/// `remote`, the `gateway` of `OpenConnect`, L2TP, PPTP, SSTP and
/// Fortinet, vpnc's `IPSec gateway`, strongSwan's `address` and
/// Libreswan's `right`.
const SERVER_KEYS: [&str; 5] = ["remote", "gateway", "IPSec gateway", "address", "right"];

/// The longest host name DNS allows.
const MAX_HOST_BYTES: usize = 253;

/// Where a VPN profile's servers are, as host names or addresses without
/// ports: where its tunnel's own packets go, which a block while it is down
/// must let through.
///
/// # Errors
/// An invalid id, or `NetworkManager` cannot be asked.
pub fn vpn_servers(uuid: &str) -> Result<Vec<String>, String> {
    validate_uuid(uuid)?;
    with_nm(|nm| async move { Ok(servers_of(&nm.profile(uuid).await?.settings)) })
}

/// The host of a server as a profile writes it: `host`, `host:port`,
/// `host:port:proto`, `[v6]:port`, a bare IPv6 address, or `host port
/// proto`.
fn server_host(text: &str) -> Option<String> {
    let first = text.split_whitespace().next()?;
    let host = if let Some(bracketed) = first.strip_prefix('[') {
        bracketed.split(']').next()?
    } else if first.parse::<std::net::IpAddr>().is_ok() {
        first
    } else {
        first.split(':').next()?
    };
    (!host.is_empty() && host.len() <= MAX_HOST_BYTES && !host.chars().any(char::is_control))
        .then(|| host.to_owned())
}

/// A profile's servers: its `WireGuard` peers' endpoints, or its plugin's
/// server keys, each a list (`OpenVPN`'s remotes) separated by commas.
fn servers_of(profile: &Settings) -> Vec<String> {
    let mut hosts: Vec<String> = profile
        .wireguard_peers()
        .into_iter()
        .filter_map(|peer| peer.endpoint)
        .filter_map(|endpoint| server_host(&endpoint))
        .collect();
    let data = profile.string_map("vpn", "data");
    for key in SERVER_KEYS {
        if let Some(servers) = data.get(key) {
            hosts.extend(servers.split(',').filter_map(server_host));
        }
    }
    let mut seen = std::collections::HashSet::new();
    hosts.retain(|host| seen.insert(host.clone()));
    hosts
}

/// What to change of a VPN profile; `None` leaves a value as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VpnProfilePatch {
    pub name: Option<String>,
    pub autoconnect: Option<bool>,
    pub all_traffic: Option<bool>,
    pub dns: Option<Vec<String>>,
    pub mtu: Option<Option<u32>>,
    pub endpoint: Option<String>,
    pub allowed_ips: Option<Vec<String>>,
    pub keepalive: Option<Option<u32>>,
}

impl VpnProfilePatch {
    /// Whether it changes a `WireGuard` peer.
    fn touches_peer(&self) -> bool {
        self.endpoint.is_some() || self.allowed_ips.is_some() || self.keepalive.is_some()
    }
}

fn valid_endpoint(endpoint: &str) -> bool {
    valid_proxy_server(endpoint)
}

/// Change a VPN profile.
///
/// # Errors
/// An invalid id or value, or `NetworkManager` refusing.
pub fn configure_vpn_profile(uuid: &str, patch: &VpnProfilePatch) -> Result<(), String> {
    validate_uuid(uuid)?;
    let patch = patch.clone();
    with_nm(|nm| async move {
        nm.edit(uuid, |settings| {
            let current = vpn_settings_of(settings)?;
            apply_vpn_patch(settings, &current, &patch)
        })
        .await
    })
}

fn apply_vpn_patch(
    settings: &mut Settings,
    current: &VpnProfileSettings,
    patch: &VpnProfilePatch,
) -> Result<(), String> {
    if let Some(name) = &patch.name {
        if name.trim().is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
            return Err("a VPN's name is 1 to 255 characters".into());
        }
        settings.set("connection", "id", name.trim());
    }
    if let Some(on) = patch.autoconnect {
        settings.set("connection", "autoconnect", on);
    }
    if let Some(all) = patch.all_traffic {
        settings.set("ipv4", "never-default", !all);
        settings.set("ipv6", "never-default", !all);
    }
    if let Some(dns) = &patch.dns {
        if dns.len() > 16 || dns.iter().any(|v| !valid_ip_token(v)) {
            return Err("invalid DNS server".into());
        }
        settings.set_dns(IpFamily::V4, dns)?;
        settings.set("ipv4", "ignore-auto-dns", !dns.is_empty());
    }
    let mut data = settings.string_map("vpn", "data");
    let plugin = current.kind != "wireguard";
    if let Some(mtu) = patch.mtu {
        if mtu.is_some_and(|mtu| !(576..=9000).contains(&mtu)) {
            return Err("an MTU is between 576 and 9000".into());
        }
        if plugin {
            match mtu {
                Some(mtu) => data.insert("tunnel-mtu".into(), mtu.to_string()),
                None => data.remove("tunnel-mtu"),
            };
        } else {
            settings.set("wireguard", "mtu", mtu.unwrap_or(0));
        }
    }
    if patch.touches_peer() {
        if plugin {
            if let Some(endpoint) = &patch.endpoint {
                if !valid_endpoint(endpoint) {
                    return Err("a server is a host name or address and a port".into());
                }
                data.insert("remote".into(), endpoint.clone());
            }
        } else {
            let Some(public_key) = current.peer_public_key.clone() else {
                return Err("this tunnel has no server to change".into());
            };
            let endpoint = patch.endpoint.clone().or_else(|| current.endpoint.clone());
            if endpoint.as_deref().is_some_and(|e| !valid_endpoint(e)) {
                return Err("a server is a host name or address and a port".into());
            }
            let allowed_ips = patch
                .allowed_ips
                .clone()
                .unwrap_or_else(|| current.allowed_ips.clone());
            if allowed_ips.len() > 64 || allowed_ips.iter().any(|v| !valid_ip_token(v)) {
                return Err("invalid allowed address".into());
            }
            let keepalive = patch.keepalive.unwrap_or(current.keepalive);
            if keepalive.is_some_and(|seconds| seconds > 65_535) {
                return Err("a keepalive is at most 65535 seconds".into());
            }
            settings.set_wireguard_peer(&WireGuardPeer {
                public_key,
                endpoint,
                allowed_ips,
                keepalive,
                preshared_key: None,
            });
        }
    }
    if plugin && settings.has("vpn") {
        settings.set("vpn", "data", data);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// WireGuard keys and tunnels
// ---------------------------------------------------------------------------

fn base64_of(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A new `WireGuard` key pair: the private key (kept by `NetworkManager`)
/// and the public key the server is told.
///
/// # Errors
/// The system has no randomness to give.
pub fn generate_wireguard_keys() -> Result<(Zeroizing<String>, String), String> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    getrandom::fill(&mut *bytes).map_err(|error| format!("no randomness for a key: {error}"))?;
    // Clamped as `wg genkey` writes it.
    if let Some(first) = bytes.first_mut() {
        *first &= 0xF8;
    }
    if let Some(last) = bytes.last_mut() {
        *last = (*last & 0x7F) | 0x40;
    }
    let public = public_of(*bytes)?;
    Ok((Zeroizing::new(base64_of(&*bytes)), public))
}

/// The public key of the `WireGuard` private key `private`.
///
/// # Errors
/// `private` is not a base64 `WireGuard` key.
pub fn wireguard_public_key(private: &str) -> Result<String, String> {
    let decoded = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(private.trim())
            .map_err(|_| "a WireGuard key is base64".to_string())?,
    );
    let bytes: [u8; 32] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| "a WireGuard key is 32 bytes".to_string())?;
    public_of(bytes)
}

/// The X25519 public key of the private key `bytes`, in base64.
fn public_of(bytes: [u8; 32]) -> Result<String, String> {
    tilo_seal::SecretKey::from_bytes(bytes)
        .public_key()
        .map(|public| base64_of(public.as_bytes()))
        .map_err(|error| format!("not a WireGuard key: {error}"))
}

/// A `WireGuard` tunnel to set up.
pub struct WireGuardTunnel {
    pub private_key: Zeroizing<String>,
    /// This device's address in the tunnel (`10.7.0.2/32`).
    pub address: String,
    pub dns: Vec<String>,
    pub peer_public_key: String,
    pub endpoint: String,
    pub allowed_ips: Vec<String>,
    pub keepalive: Option<u32>,
}

impl WireGuardTunnel {
    /// The tunnel as a `wg-quick` configuration file.
    ///
    /// # Errors
    /// A value that is not what its field holds.
    pub fn config(&self) -> Result<Zeroizing<String>, String> {
        if !valid_ip_token(&self.address) || !self.address.contains('/') {
            return Err("the tunnel address is an address with its prefix: 10.7.0.2/32".into());
        }
        if !valid_endpoint(&self.endpoint) {
            return Err("a server is a host name or address and a port".into());
        }
        wireguard_public_key(&self.private_key)?;
        let key_ok = base64::engine::general_purpose::STANDARD
            .decode(self.peer_public_key.trim())
            .is_ok_and(|key| key.len() == 32);
        if !key_ok {
            return Err("the server's public key is a base64 WireGuard key".into());
        }
        if self.allowed_ips.is_empty()
            || self
                .allowed_ips
                .iter()
                .chain(&self.dns)
                .any(|v| !valid_ip_token(v))
        {
            return Err("invalid allowed address or DNS server".into());
        }
        let mut text = Zeroizing::new(String::new());
        text.push_str("[Interface]\nPrivateKey = ");
        text.push_str(self.private_key.trim());
        text.push_str("\nAddress = ");
        text.push_str(&self.address);
        if !self.dns.is_empty() {
            text.push_str("\nDNS = ");
            text.push_str(&self.dns.join(", "));
        }
        text.push_str("\n\n[Peer]\nPublicKey = ");
        text.push_str(self.peer_public_key.trim());
        text.push_str("\nEndpoint = ");
        text.push_str(&self.endpoint);
        text.push_str("\nAllowedIPs = ");
        text.push_str(&self.allowed_ips.join(", "));
        if let Some(seconds) = self.keepalive.filter(|seconds| *seconds > 0) {
            let _ = write!(text, "\nPersistentKeepalive = {seconds}");
        }
        text.push('\n');
        Ok(text)
    }
}

/// An interface name for a tunnel called `name`: what `NetworkManager`
/// names a `WireGuard` profile it imports from a file of that name.
#[must_use]
pub fn interface_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let mut joined = cleaned.split_whitespace().collect::<Vec<_>>().join("-");
    joined.truncate(15);
    let joined = joined.trim_matches('-').to_string();
    if joined.is_empty() {
        "wg-tilo".into()
    } else {
        joined
    }
}

/// The most of a configuration file an import reads.
pub const MAX_VPN_CONFIG_BYTES: u64 = 1024 * 1024;

/// The kind of VPN a configuration file is by its name: `wireguard` for
/// `.conf`, `openvpn` for `.ovpn`.
#[must_use]
pub fn vpn_kind_of_file(name: &str) -> Option<&'static str> {
    let extension = std::path::Path::new(name).extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("conf") {
        Some("wireguard")
    } else if extension.eq_ignore_ascii_case("ovpn") {
        Some("openvpn")
    } else {
        None
    }
}

fn strip_config_comment(line: &str) -> &str {
    let cut = line.find(['#', ';']).unwrap_or(line.len());
    line.get(..cut).unwrap_or(line)
}

/// Whether `text` holds what a `kind` tunnel needs to be set up: a
/// `WireGuard` file its key, address and peer; an `OpenVPN` file a server
/// and a way to check it. Secrets are only looked for, never kept.
///
/// # Errors
/// What is missing.
pub fn validate_vpn_config(kind: &str, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("The file is empty.".into());
    }
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_VPN_CONFIG_BYTES {
        return Err("The file is too large to be a VPN profile.".into());
    }
    match kind {
        "wireguard" => {
            let has = |section: &str, key: &str| {
                let mut current = "";
                text.lines().any(|raw| {
                    let line = strip_config_comment(raw).trim();
                    if let Some(next) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                        current = next.trim();
                        return false;
                    }
                    current.eq_ignore_ascii_case(section)
                        && line.split_once('=').is_some_and(|(candidate, value)| {
                            candidate.trim().eq_ignore_ascii_case(key) && !value.trim().is_empty()
                        })
                })
            };
            let missing: Vec<String> = [
                ("Interface", "PrivateKey"),
                ("Interface", "Address"),
                ("Peer", "PublicKey"),
                ("Peer", "Endpoint"),
                ("Peer", "AllowedIPs"),
            ]
            .into_iter()
            .filter(|(section, key)| !has(section, key))
            .map(|(section, key)| format!("{section}.{key}"))
            .collect();
            if missing.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "This WireGuard file is missing {}.",
                    missing.join(", ")
                ))
            }
        }
        "openvpn" => {
            let lower = text.to_ascii_lowercase();
            let lines = || lower.lines().map(str::trim_start);
            if !lines().any(|line| line.starts_with("remote ")) {
                return Err("This OpenVPN file names no server.".into());
            }
            let checks_server = lower.contains("<ca>")
                || lines()
                    .any(|line| line.starts_with("ca ") || line.starts_with("peer-fingerprint "));
            if !checks_server {
                return Err(
                    "This OpenVPN file has no CA or fingerprint, so the server cannot be checked."
                        .into(),
                );
            }
            Ok(())
        }
        _ => Err("unsupported VPN type".into()),
    }
}

/// Set up the configuration `text` of `kind` (`wireguard` or `openvpn`) as
/// a profile called `name`. The new profile's id.
///
/// # Errors
/// An invalid name, kind or file, or `NetworkManager` refusing the profile.
pub fn import_vpn_text(kind: &str, name: &str, text: &str) -> Result<String, String> {
    if name.trim().is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
        return Err("a VPN's name is 1 to 255 characters".into());
    }
    let settings = vpn_settings_from(kind, name.trim(), &interface_name(name), text, None)?;
    with_nm(|nm| async move { nm.add(settings).await })
}

/// Set up the configuration file at `path` as a profile named after the
/// file, as `NetworkManager`'s own import does. The new profile's id.
///
/// # Errors
/// The file cannot be read, is not a usable `kind` file or its name is not
/// a usable interface name (`WireGuard`), or `NetworkManager` refuses it.
pub fn import_path_finding_uuid(kind: &str, path: &std::path::Path) -> Result<String, String> {
    use std::io::Read as _;
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or("the file has no usable name")?
        .to_owned();
    let file =
        std::fs::File::open(path).map_err(|error| format!("could not read the file: {error}"))?;
    let mut text = Zeroizing::new(String::new());
    file.take(MAX_VPN_CONFIG_BYTES.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|error| format!("could not read the file: {error}"))?;
    validate_vpn_config(kind, &text)?;
    if kind == "wireguard" && interface_name(&stem) != stem {
        return Err(format!(
            "a WireGuard file's name is its interface's: {stem:?} is not one (letters, digits, - and _, at most 15)"
        ));
    }
    let settings = vpn_settings_from(kind, &stem, &stem, &text, path.parent())?;
    with_nm(|nm| async move { nm.add(settings).await })
}

/// A new profile for the `kind` configuration `text`, called `id` on
/// interface `interface`. Files an `OpenVPN` configuration names are found
/// next to it (`base`).
fn vpn_settings_from(
    kind: &str,
    id: &str,
    interface: &str,
    text: &str,
    base: Option<&std::path::Path>,
) -> Result<Settings, String> {
    let mut settings = match kind {
        "wireguard" => wireguard_settings(text)?,
        "openvpn" => {
            require_plugin(OPENVPN_SERVICE)?;
            openvpn_settings(id, text, base)?
        }
        _ => return Err("unsupported VPN type".into()),
    };
    settings.set("connection", "id", id);
    if kind == "wireguard" {
        settings.set("connection", "interface-name", interface);
    }
    Ok(settings)
}

/// `NetworkManager` keeps a profile of a plugin's type but cannot connect
/// it without the plugin.
fn require_plugin(service: &str) -> Result<(), String> {
    if installed_vpn_plugins()
        .iter()
        .any(|plugin| plugin.service == service)
    {
        Ok(())
    } else {
        Err(format!(
            "{} is not installed on this device",
            vpn_service_name(service)
        ))
    }
}

/// A `wg-quick` file as a `WireGuard` profile, as `NetworkManager`'s import
/// makes it: the interface's key, port, mark and MTU, its addresses and
/// name servers, and every peer. Hooks (`PostUp` and the like) are
/// `wg-quick`'s own and are left out.
fn wireguard_settings(text: &str) -> Result<Settings, String> {
    let mut settings = Settings::default();
    settings.set("connection", "type", "wireguard");
    let (mut v4, mut v6, mut dns4, mut dns6) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut search = Vec::new();
    let mut peers: Vec<WireGuardPeer> = Vec::new();
    let mut section = String::new();
    let list = |value: &str| -> Vec<String> {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect()
    };
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_ascii_lowercase();
            if section == "peer" {
                peers.push(WireGuardPeer::default());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "this WireGuard file has a line that is no setting: {line:?}"
            ));
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        let number = || {
            value
                .parse::<u32>()
                .map_err(|_| format!("{key} is not a number: {value:?}"))
        };
        match (section.as_str(), key.as_str()) {
            ("interface", "privatekey") => {
                wireguard_public_key(value)?;
                settings.set("wireguard", "private-key", value);
                settings.set("wireguard", "private-key-flags", 0_u32);
            }
            ("interface", "listenport") => settings.set("wireguard", "listen-port", number()?),
            ("interface", "fwmark") => {
                let mark = value
                    .strip_prefix("0x")
                    .map_or_else(
                        || value.parse::<u32>().ok(),
                        |hex| u32::from_str_radix(hex, 16).ok(),
                    )
                    .ok_or_else(|| format!("FwMark is not a number: {value:?}"))?;
                settings.set("wireguard", "fwmark", mark);
            }
            ("interface", "mtu") => settings.set("wireguard", "mtu", number()?),
            ("interface", "table") if value.eq_ignore_ascii_case("off") => {
                settings.set("wireguard", "peer-routes", false);
            }
            ("interface", "address") => {
                for address in list(value) {
                    if address.contains(':') {
                        v6.push(address);
                    } else {
                        v4.push(address);
                    }
                }
            }
            ("interface", "dns") => {
                for server in list(value) {
                    match server.parse::<std::net::IpAddr>() {
                        Ok(ip) if ip.is_ipv6() => dns6.push(server),
                        Ok(_) => dns4.push(server),
                        Err(_) => search.push(server),
                    }
                }
            }
            ("peer", key) => {
                let peer = peers.last_mut().ok_or("a peer setting outside a [Peer]")?;
                wireguard_peer_setting(peer, key, value)?;
            }
            _ => {}
        }
    }
    wireguard_addresses(&mut settings, IpFamily::V4, &v4, &dns4, &search)?;
    wireguard_addresses(&mut settings, IpFamily::V6, &v6, &dns6, &search)?;
    for peer in &peers {
        if peer.public_key.is_empty() {
            return Err("a [Peer] has no PublicKey".into());
        }
        settings.set_wireguard_peer(peer);
    }
    Ok(settings)
}

/// A `WireGuard` profile's own addresses and name servers of `family`:
/// without an address, the family is off on the tunnel.
fn wireguard_addresses(
    settings: &mut Settings,
    family: IpFamily,
    own: &[String],
    servers: &[String],
    search: &[String],
) -> Result<(), String> {
    settings.set(
        family.setting(),
        "method",
        if own.is_empty() { "disabled" } else { "manual" },
    );
    settings.set_addresses(family, own)?;
    settings.set_dns(family, servers)?;
    if !servers.is_empty() && !search.is_empty() {
        settings.set(family.setting(), "dns-search", search.to_vec());
    }
    Ok(())
}

/// One `[Peer]` line of a `wg-quick` file.
fn wireguard_peer_setting(peer: &mut WireGuardPeer, key: &str, value: &str) -> Result<(), String> {
    match key {
        "publickey" => value.clone_into(&mut peer.public_key),
        "presharedkey" => peer.preshared_key = Some(Zeroizing::new(value.to_owned())),
        "allowedips" => peer.allowed_ips.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned),
        ),
        "endpoint" => peer.endpoint = Some(value.to_owned()),
        "persistentkeepalive" if value.eq_ignore_ascii_case("off") => peer.keepalive = None,
        "persistentkeepalive" => {
            let seconds = value
                .parse::<u32>()
                .map_err(|_| format!("PersistentKeepalive is not a number: {value:?}"))?;
            peer.keepalive = Some(seconds).filter(|seconds| *seconds > 0);
        }
        _ => {}
    }
    Ok(())
}

/// `OpenVPN`'s plugin service.
const OPENVPN_SERVICE: &str = "org.freedesktop.NetworkManager.openvpn";

/// An `OpenVPN` client file as a profile of `OpenVPN`'s plugin: its servers,
/// keys and certificates (inline ones saved as files of their own), and the
/// options the plugin knows. The plugin asks for a password when it
/// connects.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per directive the plugin takes, read top to bottom"
)]
fn openvpn_settings(
    id: &str,
    text: &str,
    base: Option<&std::path::Path>,
) -> Result<Settings, String> {
    let mut data: HashMap<String, String> = HashMap::new();
    let mut remotes: Vec<String> = Vec::new();
    let mut password = false;
    let mut inline: Option<(String, String)> = None;
    let resolve = |file: &str| -> Result<String, String> {
        let path = std::path::Path::new(file.trim_matches('"'));
        let path = match base {
            _ if path.is_absolute() => path.to_path_buf(),
            Some(base) => base.join(path),
            None => return Err(format!("{file} is not a full path")),
        };
        Ok(path.to_string_lossy().into_owned())
    };
    for raw in text.lines() {
        let line = raw.trim();
        if let Some((tag, body)) = &mut inline {
            if line == format!("</{tag}>") {
                let path = save_inline(id, tag, body)?;
                let key = match tag.as_str() {
                    "tls-auth" => "ta",
                    "secret" => "static-key",
                    other => other,
                };
                data.insert(key.into(), path);
                inline = None;
            } else {
                body.push_str(raw);
                body.push('\n');
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(tag) = line.strip_prefix('<').and_then(|l| l.strip_suffix('>')) {
            if !matches!(
                tag,
                "ca" | "cert" | "key" | "tls-auth" | "tls-crypt" | "tls-crypt-v2" | "secret"
            ) {
                return Err(format!(
                    "this OpenVPN file carries a <{tag}> block the plugin does not take"
                ));
            }
            inline = Some((tag.to_owned(), String::new()));
            continue;
        }
        let mut words = line.split_whitespace();
        let directive = words.next().unwrap_or_default().trim_start_matches("--");
        let args: Vec<&str> = words.collect();
        let first = args.first().copied().unwrap_or_default();
        let mut set = |key: &str, value: &str| {
            data.insert(key.to_owned(), value.to_owned());
        };
        match directive {
            "remote" => {
                let mut remote = first.to_owned();
                for part in args.iter().skip(1).take(2) {
                    remote.push(':');
                    remote.push_str(part);
                }
                remotes.push(remote);
            }
            "port" | "rport" => set("port", first),
            "proto" => set(
                "proto-tcp",
                if first.starts_with("tcp") {
                    "yes"
                } else {
                    "no"
                },
            ),
            "dev" => {
                set("dev", first);
                if first.starts_with("tap") {
                    set("tap-dev", "yes");
                }
            }
            "dev-type" => set("dev-type", first),
            "ca" | "cert" | "key" | "tls-crypt" | "tls-crypt-v2" => {
                set(directive, &resolve(first)?);
            }
            "pkcs12" => {
                let path = resolve(first)?;
                for key in ["ca", "cert", "key"] {
                    set(key, &path);
                }
            }
            "tls-auth" => {
                set("ta", &resolve(first)?);
                if let Some(direction) = args.get(1) {
                    set("ta-dir", direction);
                }
            }
            "secret" => {
                set("static-key", &resolve(first)?);
                if let Some(direction) = args.get(1) {
                    set("static-key-direction", direction);
                }
            }
            "key-direction" => {
                set("ta-dir", first);
                set("static-key-direction", first);
            }
            "auth-user-pass" => password = true,
            "cipher" => set("cipher", first),
            "data-ciphers" => set("data-ciphers", first),
            "auth" => set("auth", first),
            "tls-cipher" => set("tls-cipher", first),
            "tls-version-min" => set("tls-version-min", first),
            "tls-version-max" => set("tls-version-max", first),
            "comp-lzo" => set(
                "comp-lzo",
                match first {
                    "no" => "no-by-default",
                    "adaptive" => "adaptive",
                    _ => "yes",
                },
            ),
            "compress" => set("compress", if first.is_empty() { "yes" } else { first }),
            "remote-cert-tls" => set("remote-cert-tls", first),
            "verify-x509-name" => {
                let kind = args.get(1).copied().unwrap_or("subject");
                set(
                    "verify-x509-name",
                    &format!("{kind}:{}", first.trim_matches('\'')),
                );
            }
            "tun-mtu" => set("tunnel-mtu", first),
            "fragment" => set("fragment-size", first),
            "mssfix" => set("mssfix", if first.is_empty() { "yes" } else { first }),
            "reneg-sec" => set("reneg-seconds", first),
            "float" => set("float", "yes"),
            "remote-random" => set("remote-random", "yes"),
            "ping" => set("ping", first),
            "ping-restart" => set("ping-restart", first),
            "ping-exit" => set("ping-exit", first),
            "keepalive" => {
                set("ping", first);
                if let Some(restart) = args.get(1) {
                    set("ping-restart", restart);
                }
            }
            "connect-timeout" | "server-poll-timeout" => set("connect-timeout", first),
            "http-proxy" => {
                set("proxy-type", "http");
                set("proxy-server", first);
                set("proxy-port", args.get(1).copied().unwrap_or("8080"));
            }
            "socks-proxy" => {
                set("proxy-type", "socks");
                set("proxy-server", first);
                set("proxy-port", args.get(1).copied().unwrap_or("1080"));
            }
            // What every client file says and the plugin does on its own.
            _ => {}
        }
    }
    if inline.is_some() {
        return Err("this OpenVPN file has a block that is never closed".into());
    }
    if remotes.is_empty() {
        return Err("This OpenVPN file names no server.".into());
    }
    data.insert("remote".into(), remotes.join(", "));
    let keys = data.contains_key("cert") && data.contains_key("key");
    let kind = if data.contains_key("static-key") {
        "static-key"
    } else if password && keys {
        "password-tls"
    } else if password {
        "password"
    } else {
        "tls"
    };
    data.insert("connection-type".into(), kind.into());
    let mut settings = Settings::default();
    settings.set("connection", "type", "vpn");
    settings.set("vpn", "service-type", OPENVPN_SERVICE);
    settings.set("vpn", "data", data);
    Ok(settings)
}

/// Save a key or certificate an `OpenVPN` file carries inline as a file only
/// this person reads, where `OpenVPN`'s plugin keeps them; its path.
fn save_inline(id: &str, tag: &str, body: &str) -> Result<String, String> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".local/share"))
        })
        .ok_or("no data directory to keep the certificates in")?;
    let dir = data.join("networkmanagement/certificates/nm-openvpn");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|error| format!("could not keep the certificates: {error}"))?;
    let path = dir.join(format!("{}-{tag}.pem", interface_name(id)));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("could not keep the certificates: {error}"))?;
    file.write_all(body.as_bytes())
        .map_err(|error| format!("could not keep the certificates: {error}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Set up a VPN of an installed plugin's type (`service`), called `name`,
/// reaching `server`; the plugin asks for the rest when it connects. The
/// new profile's id.
///
/// # Errors
/// An unknown service, an invalid name or server, or `NetworkManager`
/// refusing.
pub fn create_plugin_vpn(service: &str, name: &str, server: &str) -> Result<String, String> {
    if !installed_vpn_plugins()
        .iter()
        .any(|plugin| plugin.service == service)
    {
        return Err("this VPN type is not installed".into());
    }
    if name.trim().is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
        return Err("a VPN's name is 1 to 255 characters".into());
    }
    let host = server.rsplit_once(':').map_or(server, |(host, _)| host);
    if server.trim().is_empty()
        || server.len() > 255
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_' | '[' | ']'))
        || server.chars().any(|c| matches!(c, ',' | '=' | ' '))
    {
        return Err("a server is a host name or address".into());
    }
    let plugin = service.rsplit('.').next().unwrap_or_default();
    let key = if plugin == "vpnc" {
        "IPSec gateway"
    } else if plugin == "openvpn" {
        "remote"
    } else {
        "gateway"
    };
    let mut settings = Settings::default();
    settings.set("connection", "id", name.trim());
    settings.set("connection", "type", "vpn");
    settings.set("vpn", "service-type", service);
    settings.set(
        "vpn",
        "data",
        HashMap::from([(key.to_owned(), server.to_owned())]),
    );
    with_nm(|nm| async move { nm.add(settings).await })
}

/// A VPN profile as the file Export writes: a `WireGuard` tunnel as a
/// `wg-quick` file with its key, an `OpenVPN` one as a client file naming
/// its certificates.
///
/// # Errors
/// An invalid id, a type that cannot be exported, or the request refused.
pub fn export_vpn(uuid: &str) -> Result<Zeroizing<String>, String> {
    validate_uuid(uuid)?;
    let profile = with_nm(|nm| async move { Ok(nm.profile(uuid).await?.settings) })?;
    match profile.string("vpn", "service-type").as_deref() {
        _ if profile.kind() == "wireguard" => export_wireguard(uuid),
        Some(OPENVPN_SERVICE) => Ok(openvpn_file(&profile.string_map("vpn", "data"))),
        Some(service) => Err(format!(
            "{} profiles cannot be exported",
            vpn_service_name(service)
        )),
        None => Err("this profile is not a VPN".into()),
    }
}

/// An `OpenVPN` profile's plugin data as a client file.
fn openvpn_file(data: &HashMap<String, String>) -> Zeroizing<String> {
    let get = |key: &str| data.get(key).map(String::as_str).filter(|v| !v.is_empty());
    let mut text = Zeroizing::new(String::from("client\n"));
    let line = |text: &mut String, words: &[&str]| {
        text.push_str(&words.join(" "));
        text.push('\n');
    };
    line(&mut text, &["dev", get("dev").unwrap_or("tun")]);
    if let Some(kind) = get("dev-type") {
        line(&mut text, &["dev-type", kind]);
    }
    line(
        &mut text,
        &[
            "proto",
            if get("proto-tcp") == Some("yes") {
                "tcp-client"
            } else {
                "udp"
            },
        ],
    );
    for remote in get("remote").unwrap_or_default().split(',') {
        let parts: Vec<&str> = remote.trim().split(':').collect();
        if let Some(host) = parts.first().filter(|host| !host.is_empty()) {
            let mut words = vec!["remote", *host];
            words.extend(parts.iter().skip(1).take(2));
            line(&mut text, &words);
        }
    }
    if let Some(port) = get("port") {
        line(&mut text, &["port", port]);
    }
    for (key, directive) in [
        ("ca", "ca"),
        ("cert", "cert"),
        ("key", "key"),
        ("tls-crypt", "tls-crypt"),
        ("tls-crypt-v2", "tls-crypt-v2"),
        ("cipher", "cipher"),
        ("data-ciphers", "data-ciphers"),
        ("auth", "auth"),
        ("tls-cipher", "tls-cipher"),
        ("tls-version-min", "tls-version-min"),
        ("tls-version-max", "tls-version-max"),
        ("remote-cert-tls", "remote-cert-tls"),
        ("tunnel-mtu", "tun-mtu"),
        ("fragment-size", "fragment"),
        ("reneg-seconds", "reneg-sec"),
        ("ping", "ping"),
        ("ping-restart", "ping-restart"),
        ("ping-exit", "ping-exit"),
        ("connect-timeout", "connect-timeout"),
    ] {
        if let Some(value) = get(key) {
            line(&mut text, &[directive, value]);
        }
    }
    if let Some(ta) = get("ta") {
        let mut words = vec!["tls-auth", ta];
        words.extend(get("ta-dir"));
        line(&mut text, &words);
    }
    if let Some(key) = get("static-key") {
        let mut words = vec!["secret", key];
        words.extend(get("static-key-direction"));
        line(&mut text, &words);
    }
    if let Some((kind, name)) = get("verify-x509-name").and_then(|v| v.split_once(':')) {
        line(&mut text, &["verify-x509-name", name, kind]);
    }
    match get("comp-lzo") {
        Some("no-by-default") => line(&mut text, &["comp-lzo", "no"]),
        Some("adaptive") => line(&mut text, &["comp-lzo", "adaptive"]),
        Some(_) => line(&mut text, &["comp-lzo", "yes"]),
        None => {}
    }
    if let Some(compress) = get("compress") {
        line(
            &mut text,
            &if compress == "yes" {
                vec!["compress"]
            } else {
                vec!["compress", compress]
            },
        );
    }
    for (key, directive) in [("float", "float"), ("remote-random", "remote-random")] {
        if get(key) == Some("yes") {
            line(&mut text, &[directive]);
        }
    }
    if matches!(get("connection-type"), Some("password" | "password-tls")) {
        line(&mut text, &["auth-user-pass"]);
    }
    text
}

/// A `WireGuard` profile as a `wg-quick` configuration file, its private key
/// included: what Export writes. Reading the key asks `NetworkManager`,
/// which asks the person to authenticate when the profile is the system's.
///
/// # Errors
/// An invalid id, a profile that is not `WireGuard`, or the request refused.
pub fn export_wireguard(uuid: &str) -> Result<Zeroizing<String>, String> {
    validate_uuid(uuid)?;
    let (settings, key) = with_nm(|nm| async move {
        let profile = nm.profile(uuid).await?;
        let settings = vpn_settings_of(&profile.settings).map_err(NmError::Refused)?;
        if settings.kind != "wireguard" {
            return Err(NmError::Refused(
                "only a WireGuard profile is exported as a file".into(),
            ));
        }
        let secrets = nm.secrets(&profile, "wireguard").await?;
        let key = Zeroizing::new(
            secrets
                .string("wireguard", "private-key")
                .unwrap_or_default()
                .trim()
                .to_owned(),
        );
        Ok((settings, key))
    })?;
    WireGuardTunnel {
        private_key: key,
        address: settings.addresses.first().cloned().unwrap_or_default(),
        dns: settings.dns,
        peer_public_key: settings.peer_public_key.unwrap_or_default(),
        endpoint: settings.endpoint.unwrap_or_default(),
        allowed_ips: settings.allowed_ips,
        keepalive: settings.keepalive,
    }
    .config()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profiles_ipv6_and_identity_are_read_from_its_settings() {
        let mut settings = Settings::default();
        settings.set("ipv6", "method", "manual");
        settings
            .set_addresses(IpFamily::V6, &["2001:db8::10/64".into()])
            .unwrap();
        settings
            .set_gateway(IpFamily::V6, Some("2001:db8::1"))
            .unwrap();
        settings.set("802-1x", "identity", "ada@example.org");
        settings.set_path("802-1x", "ca-cert", "/etc/pki/campus.pem");
        assert_eq!(
            ipv6_of(&settings),
            Some(Ipv6Configuration {
                method: Ipv6Method::Manual,
                addresses: vec!["2001:db8::10/64".into()],
                gateway: Some("2001:db8::1".into()),
                dns: Vec::new(),
            })
        );
        assert_eq!(
            enterprise_identity_of(&settings),
            Some(EnterpriseIdentity {
                identity: "ada@example.org".into(),
                ca_certificate: "/etc/pki/campus.pem".into(),
            })
        );
        assert_eq!(
            ipv6_of(&Settings::default()).map(|v6| v6.method),
            Some(Ipv6Method::Automatic),
            "NetworkManager's default"
        );
        settings.set("ipv6", "method", "shared");
        assert_eq!(ipv6_of(&settings), None, "a method this does not edit");
        assert_eq!(enterprise_identity_of(&Settings::default()), None);
    }

    #[test]
    fn one_proxy_server_round_trips_through_its_script_and_others_are_not_read_as_one() {
        let script = manual_proxy_script("proxy.example:8080");
        assert_eq!(
            manual_proxy_of(&script).as_deref(),
            Some("proxy.example:8080")
        );
        assert_eq!(
            manual_proxy_of("function FindProxyForURL() { return \"DIRECT\"; }"),
            None
        );
        assert!(valid_proxy_server("[2001:db8::1]:3128"));
        for bad in [
            "proxy.example",
            "proxy example:80",
            "host:0",
            "host:99999",
            ":80",
        ] {
            assert!(!valid_proxy_server(bad), "{bad}");
        }
        assert_eq!(WifiBand::of_frequency(5180), Some(WifiBand::Five));
        assert_eq!(WifiBand::of_frequency(2412), Some(WifiBand::TwoPointFour));
        assert_eq!(WifiBand::of_frequency(6115), Some(WifiBand::Six));
    }

    fn tunnel() -> WireGuardTunnel {
        let (private, _) = generate_wireguard_keys().unwrap();
        let (_, server) = generate_wireguard_keys().unwrap();
        WireGuardTunnel {
            private_key: private,
            address: "10.7.0.2/32".into(),
            dns: vec!["1.1.1.1".into()],
            peer_public_key: server,
            endpoint: "home.example:51820".into(),
            allowed_ips: vec!["0.0.0.0/0".into(), "::/0".into()],
            keepalive: Some(25),
        }
    }

    /// A tunnel Settings writes as a `wg-quick` file becomes the profile it
    /// describes, which reads back as the same tunnel.
    #[test]
    fn a_wireguard_file_becomes_the_profile_it_describes() {
        let tunnel = tunnel();
        let text = tunnel.config().unwrap().replace(
            "[Peer]",
            "MTU = 1380\nDNS = 2001:db8::53, corp.example\nPostUp = iptables -A FORWARD -j ACCEPT\n\n\
             [Peer]\nPresharedKey = c2VjcmV0\n",
        );
        let settings = vpn_settings_from("wireguard", "Home", "home", &text, None).unwrap();
        assert_eq!(settings.kind(), "wireguard");
        assert_eq!(settings.id(), "Home");
        assert_eq!(
            settings.string("connection", "interface-name").as_deref(),
            Some("home")
        );
        assert_eq!(
            settings.string("wireguard", "private-key").as_deref(),
            Some(tunnel.private_key.trim())
        );
        assert_eq!(settings.method(IpFamily::V6).as_deref(), Some("disabled"));
        assert_eq!(settings.dns(IpFamily::V6), ["2001:db8::53"]);
        assert_eq!(settings.strings("ipv6", "dns-search"), ["corp.example"]);
        let read = vpn_settings_of(&settings).unwrap();
        assert_eq!(read.addresses, ["10.7.0.2/32"]);
        assert_eq!(read.dns, ["1.1.1.1"]);
        assert_eq!(read.mtu, Some(1380));
        assert_eq!(read.endpoint.as_deref(), Some("home.example:51820"));
        assert_eq!(read.allowed_ips, ["0.0.0.0/0", "::/0"]);
        assert_eq!(read.keepalive, Some(25));
        assert_eq!(
            read.peer_public_key.as_deref(),
            Some(tunnel.peer_public_key.as_str())
        );
        assert!(read.all_traffic && read.autoconnect);
        let peers = settings.dicts("wireguard", "peers");
        assert!(peers[0].contains_key("preshared-key"));

        assert!(
            wireguard_settings("[Interface]\nPrivateKey = aGVsbG8=\n").is_err(),
            "a key that is no WireGuard key"
        );
        assert!(wireguard_settings("[Peer]\nEndpoint = x:1\n").is_err());
    }

    #[test]
    fn a_vpn_patch_rewrites_the_peer_whole_and_checks_its_values() {
        let mut settings = vpn_settings_from(
            "wireguard",
            "Home",
            "home",
            &tunnel().config().unwrap(),
            None,
        )
        .unwrap();
        let current = vpn_settings_of(&settings).unwrap();
        apply_vpn_patch(
            &mut settings,
            &current,
            &VpnProfilePatch {
                keepalive: Some(None),
                all_traffic: Some(false),
                mtu: Some(Some(1400)),
                ..VpnProfilePatch::default()
            },
        )
        .unwrap();
        let read = vpn_settings_of(&settings).unwrap();
        assert!(!read.all_traffic);
        assert_eq!(read.keepalive, None);
        assert_eq!(read.mtu, Some(1400));
        assert_eq!(read.endpoint, current.endpoint, "kept");
        assert_eq!(read.allowed_ips, current.allowed_ips, "kept");
        assert_eq!(settings.boolean("ipv6", "never-default"), Some(true));
        for bad in [
            VpnProfilePatch {
                mtu: Some(Some(70_000)),
                ..VpnProfilePatch::default()
            },
            VpnProfilePatch {
                endpoint: Some("no port".into()),
                ..VpnProfilePatch::default()
            },
            VpnProfilePatch {
                name: Some(" ".into()),
                ..VpnProfilePatch::default()
            },
        ] {
            assert!(
                apply_vpn_patch(&mut settings.clone(), &current, &bad).is_err(),
                "{bad:?}"
            );
        }
    }

    fn openvpn() -> Settings {
        openvpn_settings(
            "Work",
            "client\ndev tun\nproto tcp\nremote vpn.work.example 1194\nremote backup.work.example 443 tcp\n\
             ca ca.crt\ncert /etc/openvpn/me.crt\nkey /etc/openvpn/me.key\ntls-auth ta.key 1\n\
             auth-user-pass\ncipher AES-256-GCM\ntun-mtu 1400\nremote-cert-tls server\nnobind\n",
            Some(std::path::Path::new("/home/ada/vpn")),
        )
        .unwrap()
    }

    #[test]
    fn an_openvpn_file_becomes_its_plugins_profile_and_exports_back() {
        let settings = openvpn();
        let data = settings.string_map("vpn", "data");
        assert_eq!(
            data.get("remote").map(String::as_str),
            Some("vpn.work.example:1194, backup.work.example:443:tcp")
        );
        assert_eq!(
            data.get("ca").map(String::as_str),
            Some("/home/ada/vpn/ca.crt")
        );
        assert_eq!(
            data.get("ta").map(String::as_str),
            Some("/home/ada/vpn/ta.key")
        );
        assert_eq!(data.get("ta-dir").map(String::as_str), Some("1"));
        assert_eq!(
            data.get("connection-type").map(String::as_str),
            Some("password-tls")
        );
        assert_eq!(data.get("proto-tcp").map(String::as_str), Some("yes"));

        let read = vpn_settings_of(&settings).unwrap();
        assert_eq!(read.service.as_deref(), Some(OPENVPN_SERVICE));
        assert_eq!(read.mtu, Some(1400));
        assert_eq!(
            servers_of(&settings),
            ["vpn.work.example", "backup.work.example"]
        );

        let file = openvpn_file(&data);
        for line in [
            "client",
            "remote vpn.work.example 1194",
            "remote backup.work.example 443 tcp",
            "ca /home/ada/vpn/ca.crt",
            "tls-auth /home/ada/vpn/ta.key 1",
            "tun-mtu 1400",
            "auth-user-pass",
        ] {
            assert!(file.lines().any(|l| l == line), "{line}\n{}", file.as_str());
        }
        let again = openvpn_settings("Work", &file, None).unwrap();
        assert_eq!(
            again.string_map("vpn", "data").get("remote"),
            data.get("remote")
        );

        assert!(
            openvpn_settings("x", "client\nca ca.crt\nremote a 1\n", None).is_err(),
            "a relative path with no file to be next to"
        );
        assert!(openvpn_settings("x", "client\nremote a 1\n<ca>\nunclosed\n", None).is_err());
    }

    #[test]
    fn a_profiles_servers_are_its_peers_endpoints_or_its_plugins_server() {
        let mut gateway = Settings::default();
        gateway.set("connection", "type", "vpn");
        gateway.set(
            "vpn",
            "data",
            HashMap::from([
                ("gateway".to_owned(), "198.51.100.7".to_owned()),
                ("user".to_owned(), "ada".to_owned()),
            ]),
        );
        assert_eq!(servers_of(&gateway), ["198.51.100.7"]);
        gateway.set(
            "vpn",
            "data",
            HashMap::from([("tunnel-mtu".to_owned(), "1400".to_owned())]),
        );
        assert!(servers_of(&gateway).is_empty());
        let mut wireguard = Settings::default();
        wireguard.set_wireguard_peer(&WireGuardPeer {
            public_key: "AAA=".into(),
            endpoint: Some("[2001:db8::7]:51820".into()),
            ..WireGuardPeer::default()
        });
        assert_eq!(servers_of(&wireguard), ["2001:db8::7"]);
        assert_eq!(server_host("2001:db8::9"), Some("2001:db8::9".into()));
        assert_eq!(server_host(""), None);
    }

    /// RFC 7748's Alice: a known private key gives its known public key.
    #[test]
    fn a_wireguard_public_key_is_the_x25519_one() {
        let hex = |text: &str| -> Vec<u8> {
            (0..text.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
                .collect()
        };
        let private = base64_of(&hex(
            "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
        ));
        let public = base64_of(&hex(
            "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a",
        ));
        assert_eq!(wireguard_public_key(&private).unwrap(), public);
    }

    #[test]
    fn generated_wireguard_keys_pair_up_and_a_tunnel_writes_a_complete_file() {
        let (private, public) = generate_wireguard_keys().unwrap();
        assert_eq!(wireguard_public_key(&private).unwrap(), public);
        let (_, server) = generate_wireguard_keys().unwrap();
        let tunnel = WireGuardTunnel {
            private_key: private,
            address: "10.7.0.2/32".into(),
            dns: vec!["1.1.1.1".into()],
            peer_public_key: server.clone(),
            endpoint: "home.example:51820".into(),
            allowed_ips: vec!["0.0.0.0/0".into()],
            keepalive: Some(25),
        };
        let text = tunnel.config().unwrap();
        for line in [
            "[Interface]",
            "Address = 10.7.0.2/32",
            "DNS = 1.1.1.1",
            "[Peer]",
            &format!("PublicKey = {server}"),
            "Endpoint = home.example:51820",
            "AllowedIPs = 0.0.0.0/0",
            "PersistentKeepalive = 25",
        ] {
            assert!(text.lines().any(|l| l == line), "{line}");
        }
        assert_eq!(
            interface_name("Home Office VPN (Berlin)"),
            "home-office-vpn"
        );
        assert_eq!(interface_name("!!!"), "wg-tilo");
    }

    #[test]
    fn a_network_shares_as_the_wifi_scheme_phones_scan() {
        assert_eq!(
            wifi_qr_payload("Cafe; Guest", Some("pa:ss\\word"), false).as_str(),
            "WIFI:T:WPA;S:Cafe\\; Guest;P:pa\\:ss\\\\word;;"
        );
        assert_eq!(
            wifi_qr_payload("Library", None, true).as_str(),
            "WIFI:T:nopass;S:Library;H:true;;"
        );
    }

    const WIREGUARD_FILE: &str = "\
[Interface]
PrivateKey = aGVsbG8=
Address = 10.7.0.2/32
DNS = 1.1.1.1   # resolver

[Peer]
PublicKey = kP8x2yQ1mZ7vR3nL9fH0sJtW6cB4dE5gA1iU2oXyz0=
Endpoint = home.example:51820
AllowedIPs = 0.0.0.0/0, ::/0
";

    #[test]
    fn a_vpn_file_is_checked_for_what_its_tunnel_needs() {
        assert!(validate_vpn_config("wireguard", WIREGUARD_FILE).is_ok());
        let missing = validate_vpn_config("wireguard", "[Peer]\nEndpoint = x:1\n").unwrap_err();
        assert!(missing.contains("Interface.PrivateKey"), "{missing}");
        assert!(
            validate_vpn_config("wireguard", "[Interface]\n# PrivateKey = commented\n").is_err(),
            "a commented key is no key"
        );
        assert!(validate_vpn_config("openvpn", "client\nremote vpn.example 1194\n").is_err());
        assert!(
            validate_vpn_config(
                "openvpn",
                "client\nremote vpn.example 1194\n<ca>\ncertificate\n</ca>\n"
            )
            .is_ok()
        );
        assert!(validate_vpn_config("openvpn", "  ").is_err());
        assert_eq!(vpn_kind_of_file("home.conf"), Some("wireguard"));
        assert_eq!(vpn_kind_of_file("WORK.OVPN"), Some("openvpn"));
        assert_eq!(vpn_kind_of_file("notes.txt"), None);
    }

    #[test]
    fn a_vpn_plugin_is_named_by_its_name_file() {
        let plugin = parse_vpn_plugin(
            "[VPN Connection]\nname=openvpn\nservice=org.freedesktop.NetworkManager.openvpn\n",
        )
        .unwrap();
        assert_eq!(plugin.name, "OpenVPN");
        assert_eq!(plugin.service, "org.freedesktop.NetworkManager.openvpn");
        assert_eq!(parse_vpn_plugin("[VPN Connection]\nname=x\n"), None);
    }

    #[test]
    fn a_service_type_names_its_protocol_without_its_plugin() {
        assert_eq!(
            vpn_service_name("org.freedesktop.NetworkManager.openvpn"),
            "OpenVPN"
        );
        assert_eq!(
            vpn_service_name("org.freedesktop.NetworkManager.strongswan"),
            "IPsec (strongSwan)"
        );
        assert_eq!(vpn_service_name("com.example.tunnel"), "tunnel");
    }
}
