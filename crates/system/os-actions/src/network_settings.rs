//! The parts of a saved network Settings shows on a network's own page and
//! a VPN's: how the link in use stands (band, speed, addresses), the
//! profile's IPv6, proxy and 802.1X identity, its password, and a VPN
//! profile's tunnel (endpoint, routes, MTU, keepalive), all through
//! `NetworkManager`'s standard client. Secrets never travel on argv: they go
//! through a private file or `nmcli`'s stdin.

use std::fmt::Write as _;
use std::io::Write as _;

use base64::Engine as _;
use zeroize::{Zeroize as _, Zeroizing};

use crate::network::{split_terse, valid_ip_token, validate_uuid};
use crate::process::{run, run_capture, run_with_input};

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
    fn nmcli(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Manual => "manual",
            Self::LinkLocal => "link-local",
            Self::Disabled => "disabled",
        }
    }

    fn of_nmcli(value: &str) -> Option<Self> {
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

fn list_of(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "--")
        .take(17)
        .map(str::to_string)
        .collect()
}

/// The IPv6 settings in a profile's `nmcli` properties, `None` when its
/// method is one this does not edit (shared) or a value is unreadable.
pub(crate) fn ipv6_of<'v>(value: impl Fn(&str) -> &'v str) -> Option<Ipv6Configuration> {
    let method = Ipv6Method::of_nmcli(value("ipv6.method"))?;
    let addresses = list_of(value("ipv6.addresses"));
    let dns = list_of(value("ipv6.dns"));
    if addresses.len() > 16
        || dns.len() > 16
        || addresses.iter().chain(&dns).any(|v| !valid_ip_token(v))
    {
        return None;
    }
    let gateway = value("ipv6.gateway");
    Some(Ipv6Configuration {
        method,
        addresses,
        gateway: (!gateway.is_empty() && gateway != "--").then(|| gateway.to_string()),
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
    let manual = config.method == Ipv6Method::Manual;
    let addresses = if manual {
        config.addresses.join(",")
    } else {
        String::new()
    };
    let gateway = if manual {
        config.gateway.as_deref().unwrap_or("")
    } else {
        ""
    };
    let dns = config.dns.join(",");
    run(
        "nmcli",
        &[
            "connection",
            "modify",
            "uuid",
            uuid,
            "ipv6.method",
            config.method.nmcli(),
            "ipv6.addresses",
            &addresses,
            "ipv6.gateway",
            gateway,
            "ipv6.dns",
            &dns,
            "ipv6.ignore-auto-dns",
            if config.dns.is_empty() { "no" } else { "yes" },
        ],
    )
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
    let (v6, v4): (Vec<&String>, Vec<&String>) =
        servers.iter().partition(|server| server.contains(':'));
    let join = |list: &[&String]| {
        list.iter()
            .map(|server| server.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    let own = |list: &[&String]| if list.is_empty() { "no" } else { "yes" };
    run(
        "nmcli",
        &[
            "connection",
            "modify",
            "uuid",
            uuid,
            "ipv4.dns",
            &join(&v4),
            "ipv4.ignore-auto-dns",
            own(&v4),
            "ipv6.dns",
            &join(&v6),
            "ipv6.ignore-auto-dns",
            own(&v6),
        ],
    )
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

pub(crate) fn enterprise_identity_of<'v>(
    value: impl Fn(&str) -> &'v str,
) -> Option<EnterpriseIdentity> {
    let identity = value("802-1x.identity");
    if identity.is_empty() || identity == "--" {
        return None;
    }
    let ca = value("802-1x.ca-cert");
    let ca = ca.strip_prefix("file://").unwrap_or(ca);
    Some(EnterpriseIdentity {
        identity: identity.to_string(),
        ca_certificate: if ca == "--" {
            String::new()
        } else {
            ca.to_string()
        },
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
    run(
        "nmcli",
        &[
            "connection",
            "modify",
            "uuid",
            uuid,
            "802-1x.identity",
            &identity.identity,
            "802-1x.ca-cert",
            &identity.ca_certificate,
        ],
    )
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
    let devices = run_capture(
        "nmcli",
        &["-t", "-f", "DEVICE,TYPE,STATE", "device", "status"],
    )?;
    let device = connected_wifi_device(&devices).ok_or("no Wi-Fi device is connected")?;
    let shown = run_capture(
        "nmcli",
        &[
            "-t",
            "-f",
            "GENERAL.HWADDR,IP4.ADDRESS,IP4.GATEWAY,IP4.DNS,IP6.ADDRESS,IP6.DNS",
            "device",
            "show",
            &device,
        ],
    )?;
    let networks = run_capture(
        "nmcli",
        &[
            "-t",
            "-f",
            "IN-USE,FREQ,RATE",
            "device",
            "wifi",
            "list",
            "ifname",
            &device,
            "--rescan",
            "no",
        ],
    )
    .unwrap_or_default();
    let mut details = parse_device_show(&shown);
    details.device = device;
    if let Some((frequency, rate)) = parse_in_use_link(&networks) {
        details.frequency_mhz = frequency;
        details.rate = rate;
    }
    Ok(details)
}

fn connected_wifi_device(status: &str) -> Option<String> {
    status.lines().find_map(|line| {
        let fields = split_terse(line);
        match fields.as_slice() {
            [device, kind, state, ..] if kind == "wifi" && state == "connected" => {
                Some(device.clone())
            }
            _ => None,
        }
    })
}

fn parse_device_show(out: &str) -> WifiLinkDetails {
    let mut details = WifiLinkDetails::default();
    for line in out.lines() {
        let fields = split_terse(line);
        let Some((key, rest)) = fields.split_first() else {
            continue;
        };
        let value = rest.join(":");
        let value = value.trim();
        if value.is_empty() || value == "--" {
            continue;
        }
        if key == "GENERAL.HWADDR" {
            details.hardware_address = Some(value.to_string());
        } else if key.starts_with("IP4.ADDRESS") {
            details.ipv4.push(value.to_string());
        } else if key == "IP4.GATEWAY" {
            details.gateway = Some(value.to_string());
        } else if key.starts_with("IP4.DNS") || key.starts_with("IP6.DNS") {
            details.dns.push(value.to_string());
        } else if key.starts_with("IP6.ADDRESS") {
            details.ipv6.push(value.to_string());
        }
    }
    details
}

fn parse_in_use_link(out: &str) -> Option<(Option<u32>, Option<String>)> {
    out.lines().find_map(|line| {
        let fields = split_terse(line);
        let [in_use, frequency, rate, ..] = fields.as_slice() else {
            return None;
        };
        (in_use.trim() == "*").then(|| {
            let frequency = frequency
                .split_whitespace()
                .next()
                .and_then(|mhz| mhz.parse().ok());
            let rate = rate.trim();
            (frequency, (!rate.is_empty()).then(|| rate.to_string()))
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
    let out = Zeroizing::new(run_capture(
        "nmcli",
        &[
            "--show-secrets",
            "-g",
            "802-11-wireless-security.psk",
            "connection",
            "show",
            "uuid",
            uuid,
        ],
    )?);
    let password = Zeroizing::new(out.trim_end_matches('\n').replace("\\:", ":"));
    if password.is_empty() {
        return Err("this network has no saved password".into());
    }
    Ok(password)
}

/// Change a saved Wi-Fi network's password. The password goes to `nmcli`'s
/// editor on stdin, never on argv.
///
/// # Errors
/// An invalid id or password, or `NetworkManager` refusing.
pub fn set_wifi_profile_password(uuid: &str, password: &str) -> Result<(), String> {
    validate_uuid(uuid)?;
    if !(8..=63).contains(&password.len()) || password.chars().any(char::is_control) {
        return Err("a Wi-Fi password is 8 to 63 characters, without line breaks".into());
    }
    let script = Zeroizing::new(format!(
        "set 802-11-wireless-security.psk {password}\nsave persistent\nquit\n"
    ));
    run_with_input(
        "nmcli",
        &["connection", "edit", "uuid", uuid],
        script.as_bytes(),
        std::time::Duration::from_secs(15),
    )
    .map(drop)
}

/// Join a network that does not broadcast its name. The password, when
/// there is one, travels on `nmcli`'s `--ask` stdin.
///
/// # Errors
/// An invalid name or password, or the network refusing to be joined.
pub fn connect_hidden_wifi(ssid: &str, password: Option<&str>) -> Result<(), String> {
    if ssid.is_empty() || ssid.len() > 32 || ssid.chars().any(char::is_control) {
        return Err("a network name is 1 to 32 characters".into());
    }
    match password {
        Some(password) => {
            if password.chars().any(|c| matches!(c, '\n' | '\r')) {
                return Err("Wi-Fi password contains an invalid line break".into());
            }
            let input = Zeroizing::new(format!("{password}\n"));
            run_with_input(
                "nmcli",
                &[
                    "--ask", "--wait", "30", "device", "wifi", "connect", ssid, "hidden", "yes",
                ],
                input.as_bytes(),
                std::time::Duration::from_secs(35),
            )
            .map(drop)
        }
        None => run(
            "nmcli",
            &[
                "--wait", "30", "device", "wifi", "connect", ssid, "hidden", "yes",
            ],
        ),
    }
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
    let out = run_capture(
        "nmcli",
        &["-g", "vpn.service-type", "connection", "show", "uuid", uuid],
    )?;
    Ok(Some(out.trim().to_owned()).filter(|service| !service.is_empty()))
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

/// One `WireGuard` peer as `nmcli` writes `wireguard.peers`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Peer {
    public_key: String,
    endpoint: Option<String>,
    allowed_ips: Vec<String>,
    keepalive: Option<u32>,
}

impl Peer {
    fn parse(value: &str) -> Option<Self> {
        let mut words = value.split_whitespace();
        let mut peer = Self {
            public_key: words.next()?.to_string(),
            ..Self::default()
        };
        for word in words {
            let Some((key, value)) = word.split_once('=') else {
                continue;
            };
            match key {
                "endpoint" => peer.endpoint = Some(value.to_string()),
                "allowed-ips" => {
                    peer.allowed_ips = value
                        .split(';')
                        .map(str::trim)
                        .filter(|ip| !ip.is_empty())
                        .map(str::to_string)
                        .collect();
                }
                "persistent-keepalive" => {
                    peer.keepalive = value.parse().ok().filter(|seconds| *seconds > 0);
                }
                _ => {}
            }
        }
        Some(peer)
    }

    fn write(&self) -> String {
        let mut out = self.public_key.clone();
        if let Some(endpoint) = &self.endpoint {
            out.push_str(" endpoint=");
            out.push_str(endpoint);
        }
        if !self.allowed_ips.is_empty() {
            out.push_str(" allowed-ips=");
            out.push_str(&self.allowed_ips.join(";"));
        }
        let _ = write!(out, " persistent-keepalive={}", self.keepalive.unwrap_or(0));
        out
    }
}

/// `vpn.data`'s `key = value, key = value` pairs.
fn vpn_data(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

/// A VPN profile's settings.
///
/// # Errors
/// An invalid id, or `NetworkManager` cannot be asked.
pub fn vpn_profile_settings(uuid: &str) -> Result<VpnProfileSettings, String> {
    validate_uuid(uuid)?;
    let out = run_capture(
        "nmcli",
        &[
            "-t",
            "--escape",
            "yes",
            "-f",
            "connection.id,connection.type,connection.autoconnect,ipv4.never-default,ipv4.addresses,ipv4.dns,wireguard.mtu,wireguard.peers,vpn.service-type,vpn.data",
            "connection",
            "show",
            "uuid",
            uuid,
        ],
    )?;
    parse_vpn_profile_settings(&out)
}

fn parse_vpn_profile_settings(out: &str) -> Result<VpnProfileSettings, String> {
    if out.len() > 64 * 1024 {
        return Err("NetworkManager profile response is unexpectedly large".into());
    }
    let mut values = std::collections::HashMap::new();
    for line in out.lines() {
        let fields = split_terse(line);
        if let Some((key, rest)) = fields.split_first()
            && !rest.is_empty()
        {
            values.insert(key.clone(), rest.join(":"));
        }
    }
    let value = |key: &str| {
        values
            .get(key)
            .map_or("", String::as_str)
            .trim()
            .trim_start_matches("--")
    };
    let kind = value("connection.type").to_string();
    if !matches!(kind.as_str(), "wireguard" | "vpn") {
        return Err("this profile is not a VPN".into());
    }
    let mut settings = VpnProfileSettings {
        name: value("connection.id").to_string(),
        kind,
        service: Some(value("vpn.service-type").to_string()).filter(|s| !s.is_empty()),
        autoconnect: matches!(value("connection.autoconnect"), "yes" | "true"),
        all_traffic: !matches!(value("ipv4.never-default"), "yes" | "true"),
        addresses: list_of(value("ipv4.addresses")),
        dns: list_of(value("ipv4.dns")),
        mtu: value("wireguard.mtu").parse().ok().filter(|mtu| *mtu > 0),
        ..VpnProfileSettings::default()
    };
    if let Some(peer) = value("wireguard.peers").split(',').find_map(Peer::parse) {
        settings.endpoint = peer.endpoint;
        settings.allowed_ips = peer.allowed_ips;
        settings.peer_public_key = Some(peer.public_key);
        settings.keepalive = peer.keepalive;
    }
    for (key, data) in vpn_data(value("vpn.data")) {
        match key.as_str() {
            "remote" => settings.endpoint = Some(data),
            "tunnel-mtu" => settings.mtu = data.parse().ok(),
            _ => {}
        }
    }
    Ok(settings)
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
    let current = vpn_profile_settings(uuid)?;
    let changes = vpn_patch_arguments(&current, patch)?;
    if changes.is_empty() {
        return Ok(());
    }
    let mut argv: Vec<&str> = vec!["connection", "modify", "uuid", uuid];
    argv.extend(changes.iter().map(String::as_str));
    run("nmcli", &argv)
}

fn vpn_patch_arguments(
    current: &VpnProfileSettings,
    patch: &VpnProfilePatch,
) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut set = |key: &str, value: String| {
        args.push(key.to_string());
        args.push(value);
    };
    if let Some(name) = &patch.name {
        if name.trim().is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
            return Err("a VPN's name is 1 to 255 characters".into());
        }
        set("connection.id", name.trim().to_string());
    }
    if let Some(on) = patch.autoconnect {
        set(
            "connection.autoconnect",
            if on { "yes" } else { "no" }.into(),
        );
    }
    if let Some(all) = patch.all_traffic {
        let never = if all { "no" } else { "yes" };
        set("ipv4.never-default", never.into());
        set("ipv6.never-default", never.into());
    }
    if let Some(dns) = &patch.dns {
        if dns.len() > 16 || dns.iter().any(|v| !valid_ip_token(v)) {
            return Err("invalid DNS server".into());
        }
        set("ipv4.dns", dns.join(","));
        set(
            "ipv4.ignore-auto-dns",
            if dns.is_empty() { "no" } else { "yes" }.into(),
        );
    }
    if let Some(mtu) = patch.mtu {
        if mtu.is_some_and(|mtu| !(576..=9000).contains(&mtu)) {
            return Err("an MTU is between 576 and 9000".into());
        }
        let mtu = mtu.unwrap_or(0).to_string();
        if current.kind == "wireguard" {
            set("wireguard.mtu", mtu);
        } else {
            set("+vpn.data", format!("tunnel-mtu={mtu}"));
        }
    }
    if patch.touches_peer() {
        let endpoint = patch.endpoint.clone().or_else(|| current.endpoint.clone());
        if current.kind != "wireguard" {
            if let Some(endpoint) = &patch.endpoint {
                if !valid_endpoint(endpoint) {
                    return Err("a server is a host name or address and a port".into());
                }
                set("+vpn.data", format!("remote={endpoint}"));
            }
            return Ok(args);
        }
        let Some(public_key) = current.peer_public_key.clone() else {
            return Err("this tunnel has no server to change".into());
        };
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
        let peer = Peer {
            public_key,
            endpoint,
            allowed_ips,
            keepalive,
        };
        set("wireguard.peers", peer.write());
    }
    Ok(args)
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

/// Import the configuration `text` of `kind` (`wireguard` or `openvpn`) as
/// a profile called `name`, through a private file that is removed before
/// this returns. The new profile's id.
///
/// # Errors
/// An invalid name or kind, the file could not be written, or
/// `NetworkManager` refused it or made a profile it cannot be told apart.
pub fn import_vpn_text(kind: &str, name: &str, text: &str) -> Result<String, String> {
    if name.trim().is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
        return Err("a VPN's name is 1 to 255 characters".into());
    }
    let extension = match kind {
        "wireguard" => "conf",
        "openvpn" => "ovpn",
        _ => return Err("unsupported VPN type".into()),
    };
    let dir = private_dir()?;
    let path = dir
        .path
        .join(format!("{}.{extension}", interface_name(name)));
    write_private(&path, text)?;
    let imported = import_path_finding_uuid(kind, &path);
    drop(dir);
    let uuid = imported?;
    run(
        "nmcli",
        &[
            "connection",
            "modify",
            "uuid",
            &uuid,
            "connection.id",
            name.trim(),
        ],
    )?;
    Ok(uuid)
}

/// Import the configuration file at `path` and find the exact profile it
/// made: the one id that was not there before.
///
/// # Errors
/// `NetworkManager` refused it, or made zero or several new profiles.
pub fn import_path_finding_uuid(kind: &str, path: &std::path::Path) -> Result<String, String> {
    let known: std::collections::HashSet<String> = crate::network::list_vpn_connections()?
        .into_iter()
        .map(|profile| profile.uuid)
        .collect();
    crate::network::import_vpn_config(kind, &path.to_string_lossy())?;
    let new: Vec<String> = crate::network::list_vpn_connections()?
        .into_iter()
        .map(|profile| profile.uuid)
        .filter(|uuid| !known.contains(uuid) && !uuid.is_empty())
        .collect();
    match new.as_slice() {
        [uuid] => Ok(uuid.clone()),
        _ => Err(
            "NetworkManager imported the profile, but its new id could not be told apart; \
                  check the VPN list before importing again"
                .into(),
        ),
    }
}

/// A directory only this user reads, removed with everything in it when
/// dropped.
struct PrivateDir {
    path: std::path::PathBuf,
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        if let Ok(entries) = std::fs::read_dir(&self.path) {
            for entry in entries.flatten() {
                // Overwrite before unlinking: the file held key material.
                if let Ok(meta) = entry.metadata()
                    && let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(entry.path())
                {
                    let zeros = vec![0_u8; usize::try_from(meta.len()).unwrap_or(0)];
                    let _ = file.write_all(&zeros);
                }
                let _ = std::fs::remove_file(entry.path());
            }
        }
        let _ = std::fs::remove_dir(&self.path);
    }
}

fn private_dir() -> Result<PrivateDir, String> {
    use std::os::unix::fs::DirBuilderExt as _;
    let mut suffix = [0_u8; 8];
    getrandom::fill(&mut suffix).map_err(|error| error.to_string())?;
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let path = base.join(format!(
        "tilo-vpn-{}",
        suffix.iter().fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .map_err(|error| format!("could not make a private directory: {error}"))?;
    Ok(PrivateDir { path })
}

fn write_private(path: &std::path::Path, text: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("could not write the profile: {error}"))?;
    file.write_all(text.as_bytes())
        .map_err(|error| format!("could not write the profile: {error}"))
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
    let known: std::collections::HashSet<String> = crate::network::list_vpn_connections()?
        .into_iter()
        .map(|profile| profile.uuid)
        .collect();
    run(
        "nmcli",
        &[
            "connection",
            "add",
            "type",
            "vpn",
            "con-name",
            name.trim(),
            "vpn-type",
            service,
            "vpn.data",
            &format!("{key}={server}"),
        ],
    )?;
    crate::network::list_vpn_connections()?
        .into_iter()
        .map(|profile| profile.uuid)
        .find(|uuid| !known.contains(uuid))
        .ok_or_else(|| "NetworkManager made the profile, but it could not be found".into())
}

/// A VPN profile as the file Export writes: a `WireGuard` tunnel as a
/// `wg-quick` file with its key, any other as `NetworkManager` exports it
/// for its plugin.
///
/// # Errors
/// An invalid id, a plugin that cannot export, or the request refused.
pub fn export_vpn(uuid: &str) -> Result<Zeroizing<String>, String> {
    validate_uuid(uuid)?;
    if vpn_profile_settings(uuid)?.kind == "wireguard" {
        return export_wireguard(uuid);
    }
    run_capture("nmcli", &["connection", "export", "uuid", uuid]).map(Zeroizing::new)
}

/// A `WireGuard` profile as a `wg-quick` configuration file, its private key
/// included: what Export writes. Reading the key asks `NetworkManager`,
/// which asks the person to authenticate when the profile is the system's.
///
/// # Errors
/// An invalid id, a profile that is not `WireGuard`, or the request refused.
pub fn export_wireguard(uuid: &str) -> Result<Zeroizing<String>, String> {
    validate_uuid(uuid)?;
    let settings = vpn_profile_settings(uuid)?;
    if settings.kind != "wireguard" {
        return Err("only a WireGuard profile is exported as a file".into());
    }
    let mut private = Zeroizing::new(run_capture(
        "nmcli",
        &[
            "--show-secrets",
            "-g",
            "wireguard.private-key",
            "connection",
            "show",
            "uuid",
            uuid,
        ],
    )?);
    let key = Zeroizing::new(private.trim().to_string());
    private.zeroize();
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
    fn a_profiles_ipv6_proxy_and_identity_parse_from_its_properties() {
        let values = std::collections::HashMap::from([
            ("ipv6.method", "manual"),
            ("ipv6.addresses", "2001:db8::10/64"),
            ("ipv6.gateway", "2001:db8::1"),
            ("ipv6.dns", "--"),
            ("802-1x.identity", "ada@example.org"),
            ("802-1x.ca-cert", "file:///etc/pki/campus.pem"),
        ]);
        let value = |key: &str| -> &'static str { values.get(key).copied().unwrap_or("") };
        assert_eq!(
            ipv6_of(value),
            Some(Ipv6Configuration {
                method: Ipv6Method::Manual,
                addresses: vec!["2001:db8::10/64".into()],
                gateway: Some("2001:db8::1".into()),
                dns: Vec::new(),
            })
        );
        assert_eq!(
            enterprise_identity_of(value),
            Some(EnterpriseIdentity {
                identity: "ada@example.org".into(),
                ca_certificate: "/etc/pki/campus.pem".into(),
            })
        );
        let shared =
            |key: &str| -> &'static str { if key == "ipv6.method" { "shared" } else { "" } };
        assert_eq!(ipv6_of(shared), None, "a method this does not edit");
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
    }

    #[test]
    fn the_link_in_use_reads_its_band_rate_and_addresses() {
        assert_eq!(
            connected_wifi_device("lo:loopback:connected (externally)\nwlan0:wifi:connected\n"),
            Some("wlan0".into())
        );
        let details = parse_device_show(concat!(
            "GENERAL.HWADDR:AA\\:BB\\:CC\\:DD\\:EE\\:FF\n",
            "IP4.ADDRESS[1]:192.168.1.20/24\n",
            "IP4.GATEWAY:192.168.1.1\n",
            "IP4.DNS[1]:192.168.1.1\n",
            "IP6.ADDRESS[1]:fe80\\:\\:1/64\n",
            "IP6.DNS[1]:--\n",
        ));
        assert_eq!(
            details.hardware_address.as_deref(),
            Some("AA:BB:CC:DD:EE:FF")
        );
        assert_eq!(details.ipv4, ["192.168.1.20/24"]);
        assert_eq!(details.ipv6, ["fe80::1/64"]);
        assert_eq!(details.gateway.as_deref(), Some("192.168.1.1"));
        assert_eq!(details.dns, ["192.168.1.1"]);
        assert_eq!(
            parse_in_use_link(" :2412 MHz:54 Mbit/s\n*:5180 MHz:866 Mbit/s\n"),
            Some((Some(5180), Some("866 Mbit/s".into())))
        );
        assert_eq!(WifiBand::of_frequency(5180), Some(WifiBand::Five));
        assert_eq!(WifiBand::of_frequency(2412), Some(WifiBand::TwoPointFour));
        assert_eq!(WifiBand::of_frequency(6115), Some(WifiBand::Six));
    }

    #[test]
    fn a_wireguard_profile_reads_its_peer_and_a_patch_writes_it_back_whole() {
        let parsed = parse_vpn_profile_settings(concat!(
            "connection.id:Home\n",
            "connection.type:wireguard\n",
            "connection.autoconnect:no\n",
            "ipv4.never-default:no\n",
            "ipv4.addresses:10.7.0.2/32\n",
            "ipv4.dns:1.1.1.1\n",
            "wireguard.mtu:1380\n",
            "wireguard.peers:kP8x2yQ1mZ7vR3nL9fH0sJtW6cB4dE5gA1iU2oXyz0E= endpoint=home.example\\:51820 allowed-ips=0.0.0.0/0;\\:\\:/0 persistent-keepalive=25\n",
        ))
        .unwrap();
        assert_eq!(parsed.name, "Home");
        assert!(parsed.all_traffic && !parsed.autoconnect);
        assert_eq!(parsed.mtu, Some(1380));
        assert_eq!(parsed.endpoint.as_deref(), Some("home.example:51820"));
        assert_eq!(parsed.allowed_ips, ["0.0.0.0/0", "::/0"]);
        assert_eq!(parsed.keepalive, Some(25));
        let args = vpn_patch_arguments(
            &parsed,
            &VpnProfilePatch {
                keepalive: Some(None),
                all_traffic: Some(false),
                ..VpnProfilePatch::default()
            },
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "ipv4.never-default",
                "yes",
                "ipv6.never-default",
                "yes",
                "wireguard.peers",
                "kP8x2yQ1mZ7vR3nL9fH0sJtW6cB4dE5gA1iU2oXyz0E= endpoint=home.example:51820 \
                 allowed-ips=0.0.0.0/0;::/0 persistent-keepalive=0",
            ]
        );
        assert!(
            vpn_patch_arguments(
                &parsed,
                &VpnProfilePatch {
                    mtu: Some(Some(70_000)),
                    ..VpnProfilePatch::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn an_openvpn_profile_reads_its_server_and_mtu_from_its_plugin_data() {
        let parsed = parse_vpn_profile_settings(concat!(
            "connection.id:Work\n",
            "connection.type:vpn\n",
            "vpn.service-type:org.freedesktop.NetworkManager.openvpn\n",
            "vpn.data:remote = vpn.work.example\\:1194, tunnel-mtu = 1400, connection-type = tls\n",
            "ipv4.never-default:yes\n",
        ))
        .unwrap();
        assert_eq!(parsed.endpoint.as_deref(), Some("vpn.work.example:1194"));
        assert_eq!(parsed.mtu, Some(1400));
        assert!(!parsed.all_traffic);
        assert!(parse_vpn_profile_settings("connection.type:802-11-wireless\n").is_err());
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
