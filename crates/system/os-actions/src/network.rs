//! Wi-Fi control via `nmcli`.

use crate::process::{run, run_capture};
use zeroize::Zeroizing;

pub const HOTSPOT_PROFILE: &str = "Tilo Hotspot";

// Six independently-meaningful named flags, not a bitset in disguise — each is read and
// written by name across `shell` and `apps/settings`. Regrouping them (e.g. into a bitflags
// type) would ripple through both crates for no behavioural gain.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent, individually named radio/availability flag read \
              across shell and apps/settings; grouping them would only relocate the same state"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RadioState {
    pub airplane_mode: bool,
    pub wifi_available: bool,
    pub wifi_enabled: bool,
    pub wwan_available: bool,
    pub wwan_enabled: bool,
    pub hotspot_active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VpnConnectionInfo {
    pub name: String,
    /// `NetworkManager`'s stable connection UUID. Consumers must use this to
    /// associate locally provisioned VPN material; names are mutable labels.
    pub uuid: String,
    pub active: bool,
    pub kind: String,
}

/// # Errors
///
/// Returns an error message if `nmcli` is not installed or the radio/device status query
/// fails.
pub fn get_radio_state() -> Result<RadioState, String> {
    let radios = run_capture("nmcli", &["-t", "-f", "WIFI,WWAN", "radio"])?;
    let devices = run_capture("nmcli", &["-t", "-f", "TYPE,STATE", "device", "status"])?;
    let active = run_capture(
        "nmcli",
        &["-t", "-f", "NAME,TYPE", "connection", "show", "--active"],
    )
    .unwrap_or_default();
    Ok(parse_radio_state(&radios, &devices, &active))
}

fn parse_radio_state(radios: &str, devices: &str, active: &str) -> RadioState {
    let values: Vec<&str> = radios.lines().flat_map(|l| l.split(':')).collect();
    let wifi_enabled = values.first().is_some_and(|v| *v == "enabled");
    let wwan_enabled = values.get(1).is_some_and(|v| *v == "enabled");
    let wifi_available = devices.lines().any(|l| l.starts_with("wifi:"));
    let wwan_available = devices
        .lines()
        .any(|l| l.starts_with("gsm:") || l.starts_with("cdma:"));
    let hotspot_active = active.lines().any(|l| {
        let fields = split_terse(l);
        fields.first().is_some_and(|v| v == HOTSPOT_PROFILE)
            && fields.get(1).is_some_and(|v| v == "802-11-wireless")
    });
    RadioState {
        airplane_mode: (wifi_available || wwan_available)
            && (!wifi_available || !wifi_enabled)
            && (!wwan_available || !wwan_enabled),
        wifi_available,
        wifi_enabled,
        wwan_available,
        wwan_enabled,
        hotspot_active,
    }
}

/// # Errors
///
/// Returns an error message if `nmcli` is not installed or the radio-toggle call fails.
pub fn set_airplane_mode(enabled: bool) -> Result<(), String> {
    if enabled {
        run("nmcli", &["radio", "all", "off"])
    } else {
        // Restore radios individually so Bluetooth remains owned by BlueZ and
        // NetworkManager never changes an unrelated subsystem.
        run("nmcli", &["radio", "wifi", "on"])?;
        let _ = run("nmcli", &["radio", "wwan", "on"]);
        Ok(())
    }
}

/// # Errors
///
/// Returns an error message if `nmcli` is not installed or the radio-toggle call fails.
pub fn set_mobile_broadband(enabled: bool) -> Result<(), String> {
    run(
        "nmcli",
        &["radio", "wwan", if enabled { "on" } else { "off" }],
    )
}

/// Activate the Tilo hotspot. `NetworkManager` creates the profile and a random
/// WPA credential on first use; Tilo never receives or logs that secret.
///
/// # Errors
///
/// Returns an error message if `nmcli` is not installed or the underlying call fails.
pub fn set_hotspot(enabled: bool) -> Result<(), String> {
    if enabled {
        let profiles = run_capture("nmcli", &["-t", "-f", "NAME", "connection", "show"])?;
        if !profiles.lines().any(|name| name == HOTSPOT_PROFILE) {
            return run(
                "nmcli",
                &["device", "wifi", "hotspot", "con-name", HOTSPOT_PROFILE],
            );
        }
    }
    run(
        "nmcli",
        &[
            "connection",
            if enabled { "up" } else { "down" },
            HOTSPOT_PROFILE,
        ],
    )
}

/// # Errors
///
/// Returns an error message if `nmcli` is not installed or the command fails.
pub fn list_vpn_connections() -> Result<Vec<VpnConnectionInfo>, String> {
    let all = run_capture(
        "nmcli",
        &["-t", "-f", "NAME,UUID,TYPE", "connection", "show"],
    )?;
    let active = run_capture(
        "nmcli",
        &["-t", "-f", "NAME", "connection", "show", "--active"],
    )
    .unwrap_or_default();
    let active: Vec<&str> = active.lines().collect();
    Ok(all
        .lines()
        .filter_map(|line| {
            let fields = split_terse(line);
            let name = fields.first()?.trim();
            let uuid = fields.get(1)?.trim();
            let kind = fields.get(2)?.trim();
            matches!(kind, "vpn" | "wireguard").then(|| VpnConnectionInfo {
                name: name.to_string(),
                uuid: uuid.to_string(),
                active: active.contains(&name),
                kind: kind.to_string(),
            })
        })
        .collect())
}

/// # Errors
///
/// Returns an error message if `name` is empty or contains a NUL/newline, or `nmcli` is not
/// installed or the command fails.
pub fn set_vpn_connection(name: &str, enabled: bool) -> Result<(), String> {
    if name.is_empty() || name.bytes().any(|b| b == 0 || b == b'\n' || b == b'\r') {
        return Err("invalid VPN connection name".into());
    }
    run(
        "nmcli",
        &["connection", if enabled { "up" } else { "down" }, name],
    )
}

/// Activate/deactivate one VPN profile by `NetworkManager`'s immutable UUID.
/// This avoids binding a security-relevant local credential association to a
/// mutable display name.
///
/// # Errors
///
/// Returns an error when `uuid` is invalid, `nmcli` is unavailable, or
/// `NetworkManager` rejects the requested transition.
pub fn set_vpn_connection_uuid(uuid: &str, enabled: bool) -> Result<(), String> {
    if uuid.is_empty() || uuid.bytes().any(|b| matches!(b, 0 | b'\n' | b'\r')) {
        return Err("invalid VPN connection UUID".into());
    }
    run(
        "nmcli",
        &[
            "connection",
            if enabled { "up" } else { "down" },
            "uuid",
            uuid,
        ],
    )
}

/// # Errors
///
/// Returns an error message if `kind` is not `wireguard` or `openvpn`, or `nmcli` is not
/// installed or the import fails.
pub fn import_vpn_config(kind: &str, path: &str) -> Result<(), String> {
    if !matches!(kind, "wireguard" | "openvpn") {
        return Err("unsupported VPN type".into());
    }
    run(
        "nmcli",
        &["connection", "import", "type", kind, "file", path],
    )
}

/// Remove one `NetworkManager` VPN profile by its exact connection name.
///
/// # Errors
///
/// Returns an error for an invalid name or when `NetworkManager` refuses the
/// deletion. The fixed argument vector cannot invoke an arbitrary command.
pub fn delete_vpn_connection(name: &str) -> Result<(), String> {
    if name.is_empty() || name.bytes().any(|b| matches!(b, 0 | b'\n' | b'\r')) {
        return Err("invalid VPN connection name".into());
    }
    run("nmcli", &["connection", "delete", name])
}

/// Remove one `NetworkManager` VPN profile by immutable UUID.
///
/// # Errors
///
/// Returns an error when `uuid` is invalid, `nmcli` is unavailable, or
/// `NetworkManager` refuses the deletion.
pub fn delete_vpn_connection_uuid(uuid: &str) -> Result<(), String> {
    if uuid.is_empty() || uuid.bytes().any(|b| matches!(b, 0 | b'\n' | b'\r')) {
        return Err("invalid VPN connection UUID".into());
    }
    run("nmcli", &["connection", "delete", "uuid", uuid])
}

/// Authentication advertised by a Wi-Fi access point.
///
/// `Unknown` is fail-closed: callers must not treat an unfamiliar security
/// suite as an ordinary password network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum WifiSecurity {
    Open,
    Wpa2,
    Wpa3,
    Owe,
    Enterprise,
    #[default]
    Unknown,
}

impl TryFrom<String> for WifiSecurity {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "open" => Ok(Self::Open),
            "wpa2" => Ok(Self::Wpa2),
            "wpa3" => Ok(Self::Wpa3),
            "owe" => Ok(Self::Owe),
            "enterprise" => Ok(Self::Enterprise),
            "unknown" => Ok(Self::Unknown),
            _ => Err(format!("unsupported Wi-Fi security mode {value:?}")),
        }
    }
}

impl From<WifiSecurity> for String {
    fn from(value: WifiSecurity) -> Self {
        match value {
            WifiSecurity::Open => "open",
            WifiSecurity::Wpa2 => "wpa2",
            WifiSecurity::Wpa3 => "wpa3",
            WifiSecurity::Owe => "owe",
            WifiSecurity::Enterprise => "enterprise",
            WifiSecurity::Unknown => "unknown",
        }
        .to_string()
    }
}

impl WifiSecurity {
    #[must_use]
    pub fn from_nmcli(value: &str) -> Self {
        let security = value.trim().to_ascii_uppercase();
        if security.is_empty() || security == "--" {
            Self::Open
        } else if security.contains("802.1X") || security.contains("EAP") {
            Self::Enterprise
        } else if security.contains("OWE") {
            Self::Owe
        } else if security.contains("WPA3") || security.contains("SAE") {
            Self::Wpa3
        } else if security.contains("WPA2") {
            Self::Wpa2
        } else {
            Self::Unknown
        }
    }

    #[must_use]
    pub fn is_secure(self) -> bool {
        !matches!(self, Self::Open)
    }

    /// Only personal WPA profiles can be recreated from an SSID and a
    /// passphrase. OWE has no secret; enterprise needs identity, EAP and CA
    /// policy; unknown modes must be reviewed first.
    #[must_use]
    pub fn supports_portable_passphrase(self) -> bool {
        matches!(self, Self::Wpa2 | Self::Wpa3)
    }

    /// Stable domain-separation byte for encrypted continuity envelopes.
    #[must_use]
    pub const fn continuity_tag(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Wpa2 => 1,
            Self::Wpa3 => 2,
            Self::Owe => 3,
            Self::Enterprise => 4,
            Self::Unknown => 255,
        }
    }
}

/// One Wi-Fi network entry returned by [`list_wifi_networks`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetworkInfo {
    pub ssid: String,
    /// Signal-strength bucket in `[0, 4]`. Mapped from the nmcli percentage via
    /// `signal_pct_to_bucket`.
    pub signal: u8,
    /// `true` when the row's SECURITY column is non-empty (i.e. not `"--"`).
    pub secure: bool,
    /// Parsed authentication mode. Unlike `secure`, this is sufficient to
    /// decide whether password continuity is valid for the network.
    pub security: WifiSecurity,
    /// `true` when the row's IN-USE column is `*`.
    pub connected: bool,
}

/// Toggle the Wi-Fi radio on or off via `nmcli radio wifi on|off`.
///
/// # Errors
///
/// Returns an error message if `nmcli` is not installed or the command fails.
pub fn set_wifi(enabled: bool) -> Result<(), String> {
    let state = if enabled { "on" } else { "off" };
    run("nmcli", &["radio", "wifi", state])
}

/// Read the Wi-Fi radio state. `nmcli radio wifi` prints `enabled` or `disabled` on stdout.
///
/// # Errors
///
/// Returns an error message if `nmcli` is not installed, the command fails, or its output is
/// neither `enabled` nor `disabled`.
pub fn get_wifi() -> Result<bool, String> {
    wifi_from_radios(&get_radio_state()?)
}

fn wifi_from_radios(radios: &RadioState) -> Result<bool, String> {
    if !radios.wifi_available {
        return Err("no Wi-Fi adapter on this machine".into());
    }
    Ok(radios.wifi_enabled)
}

#[cfg(test)]
fn parse_nmcli_radio(stdout: &str) -> Result<bool, String> {
    let line = stdout
        .lines()
        .next()
        .ok_or("nmcli radio wifi empty")?
        .trim();
    match line {
        "enabled" => Ok(true),
        "disabled" => Ok(false),
        other => Err(format!("nmcli radio unexpected: {other:?}")),
    }
}

/// Scan + list visible Wi-Fi networks via `nmcli -t -f IN-USE,SSID,SIGNAL,SECURITY device
/// wifi list`. `-t` (terse) prints one row per line, fields separated by `:`.
///
/// # Errors
///
/// Returns an error message if `nmcli` is not installed or the command fails.
pub fn list_wifi_networks() -> Result<Vec<WifiNetworkInfo>, String> {
    let out = run_capture(
        "nmcli",
        &[
            "-t",
            "-f",
            "IN-USE,SSID,SIGNAL,SECURITY",
            "device",
            "wifi",
            "list",
        ],
    )?;
    Ok(parse_nmcli_wifi_list(&out))
}

fn parse_nmcli_wifi_list(stdout: &str) -> Vec<WifiNetworkInfo> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_terse(line);
        let (Some(in_use), Some(ssid_field), Some(signal_field), Some(security_field)) =
            (fields.first(), fields.get(1), fields.get(2), fields.get(3))
        else {
            continue;
        };
        let in_use = in_use.trim();
        let ssid = ssid_field.trim().to_string();
        if ssid.is_empty() {
            continue;
        }
        let signal_pct: u8 = signal_field.trim().parse().unwrap_or(0);
        let security = WifiSecurity::from_nmcli(security_field);
        out.push(WifiNetworkInfo {
            ssid,
            signal: signal_pct_to_bucket(signal_pct),
            secure: security.is_secure(),
            security,
            connected: in_use == "*",
        });
    }
    // De-duplicate SSIDs (multiple BSSIDs broadcasting the same SSID show up as separate rows):
    // keep the strongest signal and preserve the connected flag from any row.
    let mut deduped: Vec<WifiNetworkInfo> = Vec::new();
    for entry in out {
        if let Some(existing) = deduped.iter_mut().find(|e| e.ssid == entry.ssid) {
            if entry.connected || (entry.signal > existing.signal && !existing.connected) {
                existing.security = entry.security;
                existing.secure = entry.secure;
            }
            if entry.signal > existing.signal {
                existing.signal = entry.signal;
            }
            if entry.connected {
                existing.connected = true;
            }
        } else {
            deduped.push(entry);
        }
    }
    deduped
}

/// Split a single nmcli `-t` (terse) line on unescaped `:` separators. `\:` inside a field
/// stays a literal colon.
fn split_terse(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
        {
            buf.push(next);
            chars.next();
            continue;
        }
        if c == ':' {
            out.push(std::mem::take(&mut buf));
        } else {
            buf.push(c);
        }
    }
    out.push(buf);
    out
}

/// Map nmcli's `SIGNAL` percentage `0..=100` onto the popover's signal bucket `0..=4`.
fn signal_pct_to_bucket(pct: u8) -> u8 {
    match pct {
        0..=20 => 1,
        21..=45 => 2,
        46..=70 => 3,
        _ => 4,
    }
}

/// What the machine's network connection currently is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// Nothing is connected.
    Offline,
    Wired,
    Wifi,
}

/// A one-glance summary of connectivity, for the top bar's indicator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkStatus {
    pub kind: LinkKind,
    /// The connection's name — the SSID on Wi-Fi, the profile name on Ethernet. `None` when offline.
    pub name: Option<String>,
    /// Signal-strength bucket `1..=4` on Wi-Fi; `0` on wired and offline. Same scale as
    /// [`WifiNetworkInfo::signal`].
    pub signal: u8,
}

impl LinkStatus {
    pub fn offline() -> Self {
        Self {
            kind: LinkKind::Offline,
            name: None,
            signal: 0,
        }
    }
    pub fn is_online(&self) -> bool {
        !matches!(self.kind, LinkKind::Offline)
    }
}

/// Read the active connection without triggering a scan. Deliberately **not** built on
/// [`list_wifi_networks`]: that runs `device wifi list`, which asks the driver to rescan and
/// can block a timer-driven poll thread for seconds; `device status` is a cache read.
///
/// Signal strength comes from `/proc/net/wireless` instead, one file read the kernel keeps
/// current, so the bars move as the user walks around with no nmcli involvement at all.
///
/// # Errors
///
/// Returns an error message if `nmcli` is not installed or the device-status query fails.
pub fn get_link_status() -> Result<LinkStatus, String> {
    let out = run_capture(
        "nmcli",
        &[
            "-t",
            "-f",
            "TYPE,STATE,CONNECTION,DEVICE",
            "device",
            "status",
        ],
    )?;
    let mut status = parse_nmcli_device_status(&out);
    if status.kind == LinkKind::Wifi
        && let Some(pct) = wireless_link_quality_pct()
    {
        status.signal = signal_pct_to_bucket(pct);
    }
    Ok(status)
}

/// Pick the machine's primary link out of `nmcli device status`. Ethernet wins over Wi-Fi
/// when both are up — it's the one carrying the traffic and the one whose loss matters.
fn parse_nmcli_device_status(stdout: &str) -> LinkStatus {
    let mut wifi: Option<LinkStatus> = None;
    for line in stdout.lines() {
        let f = split_terse(line);
        let (Some(kind_s), Some(state), Some(conn)) = (f.first(), f.get(1), f.get(2)) else {
            continue;
        };
        let (kind_s, state, conn) = (kind_s.trim(), state.trim(), conn.trim());
        // nmcli spells the fully-up state "connected"; "connecting (getting IP configuration)" and
        // friends aren't yet carrying traffic, so they read as offline.
        if state != "connected" {
            continue;
        }
        let name = if conn.is_empty() || conn == "--" {
            None
        } else {
            Some(conn.to_string())
        };
        match kind_s {
            "ethernet" | "bridge" | "bond" | "team" => {
                return LinkStatus {
                    kind: LinkKind::Wired,
                    name,
                    signal: 0,
                };
            }
            "wifi"
                // Remember it, but keep scanning for a wired link.
                if wifi.is_none() => {
                    // Bucket 4 until /proc/net/wireless refines it, so a connected radio never draws as "no signal".
                    wifi = Some(LinkStatus {
                        kind: LinkKind::Wifi,
                        name,
                        signal: 4,
                    });
                }
            _ => {}
        }
    }
    wifi.unwrap_or_else(LinkStatus::offline)
}

/// Best link quality across every wireless interface, as a percentage. `/proc/net/wireless`
/// reports quality on the driver's own scale (a trailing `.` on each value, most drivers
/// 0..70); a value above that is taken as already-a-percentage rather than clamped, so an
/// unusual driver degrades gracefully instead of pinning.
fn wireless_link_quality_pct() -> Option<u8> {
    let raw = std::fs::read_to_string("/proc/net/wireless").ok()?;
    parse_proc_net_wireless(&raw)
}

fn parse_proc_net_wireless(raw: &str) -> Option<u8> {
    let mut best: Option<u8> = None;
    // The first two lines are headers.
    for line in raw.lines().skip(2) {
        let Some((_iface, rest)) = line.split_once(':') else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        // status, then link quality.
        let Some(_status) = fields.next() else {
            continue;
        };
        let Some(quality) = fields.next() else {
            continue;
        };
        let Ok(q) = quality.trim_end_matches('.').parse::<f32>() else {
            continue;
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0.0..=100.0 immediately above, so the rounded value always \
                      fits in u8 with no sign loss"
        )]
        let pct = if q <= 70.0 {
            ((q / 70.0) * 100.0).round().clamp(0.0, 100.0) as u8
        } else {
            q.round().clamp(0.0, 100.0) as u8
        };
        best = Some(best.map_or(pct, |b: u8| b.max(pct)));
    }
    best
}

/// Connect to a Wi-Fi SSID. Passwords travel on nmcli's `--ask` stdin,
/// never argv (which is readable through `/proc`).
///
/// # Errors
///
/// Returns an error message if `password` contains a line break, `nmcli` is not installed, or
/// the connect attempt fails or times out.
pub fn connect_wifi(ssid: &str, password: Option<&str>) -> Result<(), String> {
    if let Some(password) = password {
        if password.chars().any(|c| matches!(c, '\n' | '\r')) {
            return Err("Wi-Fi password contains an invalid line break".into());
        }
        let input = Zeroizing::new(format!("{password}\n"));
        crate::process::run_with_input(
            "nmcli",
            &["--ask", "--wait", "30", "device", "wifi", "connect", ssid],
            input.as_bytes(),
            std::time::Duration::from_secs(35),
        )?;
        Ok(())
    } else {
        run(
            "nmcli",
            &["--wait", "30", "device", "wifi", "connect", ssid],
        )
    }
}

/// Disconnect the exact `NetworkManager` connection used for a remote
/// tethering grant. This does not forget the profile or affect other radios.
///
/// # Errors
///
/// Returns an error when `ssid` is invalid or `NetworkManager` rejects the
/// targeted disconnect.
pub fn disconnect_wifi(ssid: &str) -> Result<(), String> {
    if ssid.is_empty() || ssid.len() > 32 || ssid.chars().any(char::is_control) {
        return Err("Wi-Fi name is invalid".into());
    }
    run("nmcli", &["connection", "down", "id", ssid])
}

/// `NetworkManager`'s connectivity result, including the portal state used by
/// captive-portal helpers. Values intentionally mirror the daemon rather than
/// deriving "online" from a local address.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConnectivityState {
    #[default]
    Unknown,
    Offline,
    Portal,
    Limited,
    Full,
}

impl ConnectivityState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Connectivity not checked",
            Self::Offline => "Offline",
            Self::Portal => "Sign-in required",
            Self::Limited => "Limited connectivity",
            Self::Full => "Connected to the internet",
        }
    }
}

/// # Errors
///
/// Returns an error message if `nmcli` is not installed or the connectivity check fails.
pub fn get_connectivity() -> Result<ConnectivityState, String> {
    let out = run_capture("nmcli", &["-t", "networking", "connectivity", "check"])?;
    Ok(parse_connectivity(&out))
}

/// Return `NetworkManager`'s configured connectivity-check URI. Opening this
/// URI is the standard captive-portal flow: an intercepting network redirects
/// it to its login page. Tilo does not guess a gateway or hard-code a vendor URL.
///
/// # Errors
///
/// Returns an error message if the D-Bus runtime cannot start, `NetworkManager` is unreachable,
/// it has no connectivity-check URI configured, or the configured URI is not `http(s)://`.
pub fn captive_portal_uri() -> Result<String, String> {
    crate::block_on(async {
        let connection = zbus::Connection::system()
            .await
            .map_err(|e| format!("cannot reach NetworkManager: {e}"))?;
        let proxy = zbus::Proxy::new(
            &connection,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
        )
        .await
        .map_err(|e| format!("cannot bind NetworkManager: {e}"))?;
        let uri: String = proxy
            .get_property("ConnectivityCheckUri")
            .await
            .map_err(|e| format!("NetworkManager has no connectivity-check URI: {e}"))?;
        if !(uri.starts_with("https://") || uri.starts_with("http://")) {
            return Err("NetworkManager returned an unsafe connectivity-check URI".into());
        }
        Ok(uri)
    })
}

fn parse_connectivity(out: &str) -> ConnectivityState {
    match out.trim().to_ascii_lowercase().as_str() {
        "none" => ConnectivityState::Offline,
        "portal" => ConnectivityState::Portal,
        "limited" => ConnectivityState::Limited,
        "full" => ConnectivityState::Full,
        _ => ConnectivityState::Unknown,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedNetworkProfile {
    pub name: String,
    pub uuid: String,
    pub kind: String,
    pub autoconnect: bool,
}

/// The only `NetworkManager` profile fields eligible for personal-device
/// roaming. UUID is deliberately absent: it is a machine-local lookup
/// capability, never portable state.
pub struct PortableWifiProfileSecret {
    pub ssid: String,
    pub security: WifiSecurity,
    pub password: Zeroizing<String>,
}

/// Resolve one explicit `NetworkManager` profile into portable Wi-Fi semantics.
/// The query allowlist excludes interface names, MAC addresses, routes, DNS,
/// autoconnect state, and every other machine-specific property.
///
/// # Errors
///
/// Returns an error for malformed UUIDs, non-Wi-Fi/enterprise/open profiles,
/// missing secrets, or an unavailable `NetworkManager` command.
pub fn portable_wifi_profile_secret(uuid: &str) -> Result<PortableWifiProfileSecret, String> {
    validate_uuid(uuid)?;
    let output = Zeroizing::new(run_capture(
        "nmcli",
        &[
            "--show-secrets",
            "-g",
            "connection.type,802-11-wireless.ssid,802-11-wireless-security.key-mgmt,802-11-wireless-security.psk",
            "connection",
            "show",
            uuid,
        ],
    )?);
    parse_portable_wifi_profile(&output)
}

fn parse_portable_wifi_profile(output: &str) -> Result<PortableWifiProfileSecret, String> {
    let values = output.lines().map(str::trim).collect::<Vec<_>>();
    let [kind, ssid, key_mgmt, password] = values.as_slice() else {
        return Err("NetworkManager returned an incomplete Wi-Fi profile".into());
    };
    if *kind != "802-11-wireless" {
        return Err("only Wi-Fi profiles can be shared across devices".into());
    }
    if ssid.is_empty() || ssid.len() > runtime_wifi_ssid_limit() {
        return Err("the saved Wi-Fi profile has an invalid network name".into());
    }
    let security = match key_mgmt.to_ascii_lowercase().as_str() {
        "wpa-psk" | "wpa-none" => WifiSecurity::Wpa2,
        "sae" => WifiSecurity::Wpa3,
        "wpa-eap" | "ieee8021x" => {
            return Err("enterprise Wi-Fi identity and certificates stay device-local".into());
        }
        "none" | "owe" | "" => {
            return Err("this Wi-Fi profile has no portable passphrase".into());
        }
        _ => return Err("unrecognized Wi-Fi security cannot be shared safely".into()),
    };
    if password.is_empty() || password.len() > 512 {
        return Err("the saved Wi-Fi passphrase is missing or too large".into());
    }
    Ok(PortableWifiProfileSecret {
        ssid: (*ssid).to_string(),
        security,
        password: Zeroizing::new((*password).to_string()),
    })
}

const fn runtime_wifi_ssid_limit() -> usize {
    256
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MeteredPolicy {
    #[default]
    Automatic,
    Metered,
    Unmetered,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MacPolicy {
    Permanent,
    Random,
    #[default]
    Stable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyConfiguration {
    pub automatic_url: Option<String>,
    pub browser_only: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionPolicy {
    pub metered: MeteredPolicy,
    pub mac: Option<MacPolicy>,
    pub proxy: ProxyConfiguration,
}

/// Editable values read from a `NetworkManager` connection profile. `None`
/// means `NetworkManager` returned a value this UI does not understand; callers
/// must leave that property alone rather than replacing it with a default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionProfileSettings {
    pub metered: Option<MeteredPolicy>,
    pub mac: Option<MacPolicy>,
    pub proxy: Option<ProxyConfiguration>,
    pub ipv4: Option<IpConfiguration>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionPolicyPatch {
    pub metered: Option<MeteredPolicy>,
    pub mac: Option<MacPolicy>,
    pub proxy: Option<ProxyConfiguration>,
}

/// # Errors
///
/// Returns an error message if `uuid` fails validation, the patch's automatic proxy URL is
/// not `http(s)://` or exceeds 2048 bytes, or `nmcli` is not installed or a modify call fails.
pub fn configure_connection_policy_patch(
    uuid: &str,
    patch: &ConnectionPolicyPatch,
) -> Result<(), String> {
    validate_uuid(uuid)?;
    if let Some(url) = patch
        .proxy
        .as_ref()
        .and_then(|p| p.automatic_url.as_deref())
        && (url.len() > 2048 || !(url.starts_with("https://") || url.starts_with("http://")))
    {
        return Err("automatic proxy URL must use HTTP or HTTPS and be at most 2048 bytes".into());
    }
    if let Some(value) = patch.metered {
        run(
            "nmcli",
            &[
                "connection",
                "modify",
                "uuid",
                uuid,
                "connection.metered",
                match value {
                    MeteredPolicy::Automatic => "unknown",
                    MeteredPolicy::Metered => "yes",
                    MeteredPolicy::Unmetered => "no",
                },
            ],
        )?;
    }
    if let Some(proxy) = &patch.proxy {
        run(
            "nmcli",
            &[
                "connection",
                "modify",
                "uuid",
                uuid,
                "proxy.method",
                if proxy.automatic_url.is_some() {
                    "auto"
                } else {
                    "none"
                },
                "proxy.pac-url",
                proxy.automatic_url.as_deref().unwrap_or(""),
                "proxy.browser-only",
                if proxy.browser_only { "yes" } else { "no" },
            ],
        )?;
    }
    if let Some(mac) = patch.mac {
        run(
            "nmcli",
            &[
                "connection",
                "modify",
                "uuid",
                uuid,
                "802-11-wireless.cloned-mac-address",
                match mac {
                    MacPolicy::Permanent => "permanent",
                    MacPolicy::Random => "random",
                    MacPolicy::Stable => "stable-ssid",
                },
            ],
        )?;
    }
    Ok(())
}

/// # Errors
///
/// Returns an error message if [`configure_connection_policy_patch`] fails.
pub fn configure_connection_policy(uuid: &str, policy: &ConnectionPolicy) -> Result<(), String> {
    configure_connection_policy_patch(
        uuid,
        &ConnectionPolicyPatch {
            metered: Some(policy.metered),
            mac: policy.mac,
            proxy: Some(policy.proxy.clone()),
        },
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionDiagnostics {
    pub network_manager_state: String,
    pub connectivity: ConnectivityState,
    pub device: Option<String>,
    pub address: Option<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub recommendations: Vec<String>,
}

/// `NetworkManager` state/connectivity come directly from its D-Bus API. IP
/// detail remains NM-owned and is read through its standard client, avoiding
/// route/proc guesses and a second network model.
///
/// # Errors
///
/// Returns an error message if the D-Bus runtime cannot start or `NetworkManager` is
/// unreachable.
pub fn connection_diagnostics() -> Result<ConnectionDiagnostics, String> {
    let (state, connectivity) = crate::block_on(async {
        let c = zbus::Connection::system()
            .await
            .map_err(|e| format!("cannot reach NetworkManager: {e}"))?;
        let p = zbus::Proxy::new(
            &c,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
        )
        .await
        .map_err(|e| e.to_string())?;
        let state: u32 = p.get_property("State").await.map_err(|e| e.to_string())?;
        let connectivity: u32 = p
            .get_property("Connectivity")
            .await
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((state, connectivity))
    })?;
    let connectivity = match connectivity {
        1 => ConnectivityState::Offline,
        2 => ConnectivityState::Portal,
        3 => ConnectivityState::Limited,
        4 => ConnectivityState::Full,
        _ => ConnectivityState::Unknown,
    };
    let network_manager_state = match state {
        10 => "Asleep",
        20 => "Disconnected",
        30..=50 => "Connecting",
        60 => "Connected (local)",
        70 => "Connected (site)",
        80 => "Connected",
        _ => "Unknown",
    }
    .to_string();
    let detail = run_capture(
        "nmcli",
        &[
            "-t",
            "-f",
            "GENERAL.DEVICE,IP4.ADDRESS,IP4.GATEWAY,IP4.DNS",
            "device",
            "show",
        ],
    )
    .unwrap_or_default();
    let mut out = ConnectionDiagnostics {
        network_manager_state,
        connectivity,
        ..Default::default()
    };
    for line in detail.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value == "--" {
            continue;
        }
        if key.ends_with("DEVICE") && out.device.is_none() {
            out.device = Some(value.into());
        } else if key.contains("ADDRESS") && out.address.is_none() {
            out.address = Some(value.into());
        } else if key.ends_with("GATEWAY") && out.gateway.is_none() {
            out.gateway = Some(value.into());
        } else if key.contains("DNS") {
            out.dns.push(value.into());
        }
    }
    match out.connectivity {
        ConnectivityState::Portal => out
            .recommendations
            .push("Open the network sign-in page, then run diagnostics again.".into()),
        ConnectivityState::Limited if out.gateway.is_none() => out
            .recommendations
            .push("No default gateway was received; check DHCP or the manual IP settings.".into()),
        ConnectivityState::Limited if out.dns.is_empty() => out
            .recommendations
            .push("No DNS server was received; check DNS settings on this connection.".into()),
        ConnectivityState::Offline => out.recommendations.push(
            "No connection is active; check the cable, radio, or selected mobile SIM.".into(),
        ),
        _ => {}
    }
    Ok(out)
}

/// # Errors
///
/// Returns an error message if `nmcli` is not installed or the command fails.
pub fn list_saved_network_profiles() -> Result<Vec<SavedNetworkProfile>, String> {
    let out = run_capture(
        "nmcli",
        &[
            "-t",
            "--escape",
            "yes",
            "-f",
            "NAME,UUID,TYPE,AUTOCONNECT",
            "connection",
            "show",
        ],
    )?;
    Ok(parse_saved_profiles(&out))
}

fn parse_saved_profiles(out: &str) -> Vec<SavedNetworkProfile> {
    out.lines()
        .filter_map(|line| {
            let fields = split_terse(line);
            let [name, uuid, kind, autoconnect] = fields.as_slice() else {
                return None;
            };
            matches!(
                kind.as_str(),
                "802-11-wireless" | "802-3-ethernet" | "gsm" | "cdma"
            )
            .then(|| SavedNetworkProfile {
                name: name.clone(),
                uuid: uuid.clone(),
                kind: kind.clone(),
                autoconnect: autoconnect == "yes",
            })
        })
        .collect()
}

fn validate_uuid(uuid: &str) -> Result<(), String> {
    let valid = uuid.len() == 36
        && uuid.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        });
    valid
        .then_some(())
        .ok_or_else(|| "invalid NetworkManager profile UUID".into())
}

/// # Errors
///
/// Returns an error message if `uuid` fails validation, or `nmcli` is not installed or the
/// delete fails.
pub fn forget_network_profile(uuid: &str) -> Result<(), String> {
    validate_uuid(uuid)?;
    run("nmcli", &["connection", "delete", "uuid", uuid])
}

/// # Errors
///
/// Returns an error message if `uuid` fails validation, or `nmcli` is not installed or the
/// modify call fails.
pub fn set_network_autoconnect(uuid: &str, enabled: bool) -> Result<(), String> {
    validate_uuid(uuid)?;
    run(
        "nmcli",
        &[
            "connection",
            "modify",
            "uuid",
            uuid,
            "connection.autoconnect",
            if enabled { "yes" } else { "no" },
        ],
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IpConfiguration {
    pub automatic: bool,
    pub addresses: Vec<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
}

fn valid_ip_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || matches!(b, b'.' | b':' | b'/' | b'%'))
}

/// Apply one profile's IPv4 settings through `NetworkManager`. The UUID is the
/// stable identity; user-facing names are never used as selectors. Empty DNS
/// means "use DHCP-provided DNS".
///
/// # Errors
///
/// Returns an error message if `uuid` fails validation, any address/gateway/DNS entry is
/// invalid, manual mode has no address, or `nmcli` is not installed or the modify call fails.
pub fn configure_ipv4(uuid: &str, config: &IpConfiguration) -> Result<(), String> {
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
        return Err("invalid IP address, prefix, gateway, or DNS server".into());
    }
    if !config.automatic && config.addresses.is_empty() {
        return Err("manual IPv4 needs at least one address with a prefix".into());
    }
    let addresses = config.addresses.join(",");
    let gateway = config.gateway.as_deref().unwrap_or("");
    let dns = config.dns.join(",");
    run(
        "nmcli",
        &[
            "connection",
            "modify",
            "uuid",
            uuid,
            "ipv4.method",
            if config.automatic { "auto" } else { "manual" },
            "ipv4.addresses",
            &addresses,
            "ipv4.gateway",
            gateway,
            "ipv4.dns",
            &dns,
            "ipv4.ignore-auto-dns",
            if config.dns.is_empty() { "no" } else { "yes" },
        ],
    )
}

/// Read the properties Settings can safely edit from a profile. The typed
/// `nmcli` property view is `NetworkManager`'s standard client surface and keeps
/// this crate independent of vendor files and connection serialization.
///
/// # Errors
///
/// Returns an error message if `uuid` fails validation, `nmcli` is not installed, the query
/// fails, or the response is unexpectedly large.
pub fn connection_profile_settings(uuid: &str) -> Result<ConnectionProfileSettings, String> {
    validate_uuid(uuid)?;
    let out = run_capture(
        "nmcli",
        &[
            "-t",
            "--escape",
            "yes",
            "-f",
            "connection.metered,802-11-wireless.cloned-mac-address,proxy.method,proxy.pac-url,proxy.browser-only,ipv4.method,ipv4.addresses,ipv4.gateway,ipv4.dns",
            "connection",
            "show",
            "uuid",
            uuid,
        ],
    )?;
    parse_connection_profile_settings(&out)
}

fn parse_connection_profile_settings(out: &str) -> Result<ConnectionProfileSettings, String> {
    if out.len() > 32 * 1024 {
        return Err("NetworkManager profile response is unexpectedly large".into());
    }
    let mut values = std::collections::HashMap::new();
    for line in out.lines() {
        if line.len() > 4096 {
            return Err("NetworkManager profile property is too large".into());
        }
        let fields = split_terse(line);
        if let Some((key, rest)) = fields.split_first()
            && !rest.is_empty()
        {
            values.insert(key.clone(), rest.join(":"));
        }
    }
    let value = |key: &str| values.get(key).map_or("", String::as_str).trim();
    let metered = match value("connection.metered") {
        "unknown" => Some(MeteredPolicy::Automatic),
        "yes" | "true" | "1" => Some(MeteredPolicy::Metered),
        "no" | "false" | "0" => Some(MeteredPolicy::Unmetered),
        _ => None,
    };
    let mac = match value("802-11-wireless.cloned-mac-address") {
        "stable" | "stable-ssid" => Some(MacPolicy::Stable),
        "random" => Some(MacPolicy::Random),
        "permanent" | "preserve" => Some(MacPolicy::Permanent),
        _ => None,
    };
    let proxy = match value("proxy.method") {
        "none" => Some(ProxyConfiguration {
            automatic_url: None,
            browser_only: matches!(value("proxy.browser-only"), "yes" | "true" | "1"),
        }),
        "auto" => {
            let url = value("proxy.pac-url");
            ((url.starts_with("http://") || url.starts_with("https://")) && url.len() <= 2048).then(
                || ProxyConfiguration {
                    automatic_url: Some(url.to_string()),
                    browser_only: matches!(value("proxy.browser-only"), "yes" | "true" | "1"),
                },
            )
        }
        _ => None,
    };
    let ipv4 = match value("ipv4.method") {
        "auto" | "manual" => {
            let list = |raw: &str| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .take(17)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            };
            let addresses = list(value("ipv4.addresses"));
            let dns = list(value("ipv4.dns"));
            if addresses.len() > 16
                || dns.len() > 16
                || addresses.iter().chain(&dns).any(|v| !valid_ip_token(v))
            {
                None
            } else {
                let gateway = value("ipv4.gateway");
                let gateway = (!gateway.is_empty() && gateway != "--").then(|| gateway.to_string());
                Some(IpConfiguration {
                    automatic: value("ipv4.method") == "auto",
                    addresses,
                    gateway,
                    dns,
                })
            }
        }
        _ => None,
    };
    Ok(ConnectionProfileSettings {
        metered,
        mac,
        proxy,
        ipv4,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HotspotBand {
    #[default]
    Automatic,
    TwoPointFourGhz,
    FiveGhz,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotspotConfiguration {
    pub ssid: String,
    pub password: String,
    pub band: HotspotBand,
}

impl HotspotConfiguration {
    /// # Errors
    ///
    /// Returns an error message if `ssid` is empty, longer than 32 bytes, or contains a
    /// control character, or if `password` is outside 8–63 characters or contains a control
    /// character.
    pub fn validate(&self) -> Result<(), String> {
        if self.ssid.is_empty() || self.ssid.len() > 32 || self.ssid.chars().any(char::is_control) {
            return Err("hotspot name must be 1–32 bytes".into());
        }
        if !(8..=63).contains(&self.password.len()) || self.password.chars().any(char::is_control) {
            return Err("hotspot password must be 8–63 printable characters".into());
        }
        Ok(())
    }

    /// Standard Wi-Fi QR payload. Escaping follows the WIFI schema used by
    /// Android and iOS scanners; the plaintext is returned only on explicit
    /// request and is never logged or persisted by this crate.
    ///
    /// # Errors
    ///
    /// Returns an error message if [`Self::validate`] fails.
    pub fn qr_payload(&self) -> Result<String, String> {
        self.validate()?;
        let escape = |value: &str| {
            value
                .replace('\\', "\\\\")
                .replace(';', "\\;")
                .replace(',', "\\,")
                .replace(':', "\\:")
        };
        Ok(format!(
            "WIFI:T:WPA;S:{};P:{};;",
            escape(&self.ssid),
            escape(&self.password)
        ))
    }
}

/// # Errors
///
/// Returns an error message if `config` fails [`HotspotConfiguration::validate`], or `nmcli`
/// is not installed or a step of creating/editing/activating the hotspot profile fails.
pub fn configure_hotspot(config: &HotspotConfiguration) -> Result<(), String> {
    config.validate()?;
    let profiles = run_capture("nmcli", &["-t", "-f", "NAME", "connection", "show"])?;
    if !profiles.lines().any(|name| name == HOTSPOT_PROFILE) {
        run(
            "nmcli",
            &[
                "connection",
                "add",
                "type",
                "wifi",
                "ifname",
                "*",
                "con-name",
                HOTSPOT_PROFILE,
                "ssid",
                config.ssid.as_str(),
                "802-11-wireless.mode",
                "ap",
            ],
        )?;
    }
    let band = match config.band {
        HotspotBand::Automatic => "remove wifi.band".to_string(),
        HotspotBand::TwoPointFourGhz => "set wifi.band bg".to_string(),
        HotspotBand::FiveGhz => "set wifi.band a".to_string(),
    };
    // `nmcli connection edit` accepts values on stdin. Quoting keeps editor
    // metacharacters data, while keeping the WPA key out of `/proc/*/cmdline`.
    let quote = |value: &str| format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""));
    let script = format!(
        "set wifi.ssid {}\nset wifi.mode ap\n{band}\nset wifi-sec.key-mgmt wpa-psk\nset wifi-sec.psk {}\nsave persistent\nquit\n",
        quote(&config.ssid),
        quote(&config.password),
    );
    crate::process::run_with_input(
        "nmcli",
        &["connection", "edit", HOTSPOT_PROFILE],
        script.as_bytes(),
        std::time::Duration::from_secs(15),
    )?;
    run("nmcli", &["connection", "up", HOTSPOT_PROFILE])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotspotClient {
    pub mac: String,
    pub signal_dbm: Option<i32>,
}

/// # Errors
///
/// Returns an error message if `nmcli` or `iw` is not installed, or either command fails. An
/// unrecognised or missing hotspot interface name yields an empty list rather than an error.
pub fn list_hotspot_clients() -> Result<Vec<HotspotClient>, String> {
    let iface = run_capture(
        "nmcli",
        &[
            "-g",
            "GENERAL.DEVICES",
            "connection",
            "show",
            HOTSPOT_PROFILE,
        ],
    )?
    .trim()
    .to_string();
    if iface.is_empty()
        || iface
            .bytes()
            .any(|b| !b.is_ascii_alphanumeric() && b != b'_' && b != b'-')
    {
        return Ok(Vec::new());
    }
    let out = run_capture("iw", &["dev", &iface, "station", "dump"])?;
    Ok(parse_hotspot_clients(&out))
}

fn parse_hotspot_clients(out: &str) -> Vec<HotspotClient> {
    let mut clients = Vec::new();
    for block in out.split("Station ").skip(1) {
        let mac = block
            .split_ascii_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if mac.len() != 17 {
            continue;
        }
        let signal_dbm = block.lines().find_map(|line| {
            line.trim()
                .strip_prefix("signal:")?
                .split_ascii_whitespace()
                .next()?
                .parse()
                .ok()
        });
        clients.push(HotspotClient { mac, signal_dbm });
    }
    clients
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn networkmanager_output_parses_connectivity_profiles_hotspots_and_clients() {
        // Phase `networkmanager_connectivity_keeps_portal_distinct_from_limited`.
        {
            assert_eq!(parse_connectivity("portal\n"), ConnectivityState::Portal);
            assert_eq!(parse_connectivity("limited\n"), ConnectivityState::Limited);
            assert_eq!(parse_connectivity("full\n"), ConnectivityState::Full);
        }
        // Phase `saved_profiles_preserve_escaped_colons_and_autoconnect`.
        {
            let parsed = parse_saved_profiles(
                "Office\\: Lab:12345678-1234-1234-1234-123456789abc:802-11-wireless:yes\n",
            );
            assert_eq!(parsed.len(), 1);
            assert_eq!(parsed[0].name, "Office: Lab");
            assert!(parsed[0].autoconnect);
        }
        // Phase `hotspot_qr_escapes_schema_delimiters`.
        {
            let config = HotspotConfiguration {
                ssid: "Tilo; Guest".into(),
                password: "hello:world".into(),
                band: HotspotBand::Automatic,
            };
            assert_eq!(
                config.qr_payload().unwrap(),
                "WIFI:T:WPA;S:Tilo\\; Guest;P:hello\\:world;;"
            );
        }
        // Phase `station_dump_yields_clients_without_inventing_names`.
        {
            let clients =
                parse_hotspot_clients("Station aa:bb:cc:dd:ee:ff (on wlan0)\n\tsignal: -51 dBm\n");
            assert_eq!(clients[0].mac, "AA:BB:CC:DD:EE:FF");
            assert_eq!(clients[0].signal_dbm, Some(-51));
        }
    }

    #[test]
    fn connection_settings_are_validated_before_invoking_networkmanager() {
        // Phase `manual_ipv4_validation_rejects_shellish_and_missing_addresses`.
        {
            assert!(!valid_ip_token("1.2.3.4;reboot"));
            let config = IpConfiguration {
                automatic: false,
                ..Default::default()
            };
            // Validate the pure precondition without invoking nmcli by using a
            // syntactically valid UUID; it fails before spawning.
            let err = configure_ipv4("12345678-1234-1234-1234-123456789abc", &config).unwrap_err();
            assert!(err.contains("at least one address"));
        }
        // Phase `connection_policy_rejects_unsafe_pac_before_invoking_networkmanager`.
        {
            // This must fail during validation, before an external process can run.
            let policy = ConnectionPolicy {
                proxy: ProxyConfiguration {
                    automatic_url: Some("file:///tmp/proxy.pac".into()),
                    browser_only: false,
                },
                ..Default::default()
            };
            let err = configure_connection_policy("12345678-1234-1234-1234-123456789abc", &policy)
                .unwrap_err();
            assert!(err.contains("HTTP or HTTPS"));
        }
    }
}

/// Create a `NetworkManager` WWAN profile without handling SIM secrets in the
/// UI process. PIN/password flags are `not-saved`; `NetworkManager`'s registered
/// secret agent owns prompting and custody when activation needs them.
///
/// # Errors
///
/// Returns an error message if `name` or `apn` is invalid, or `nmcli` is not installed or a
/// step of creating/modifying the profile fails.
pub fn create_mobile_profile(name: &str, apn: &str) -> Result<(), String> {
    let safe = |v: &str, max: usize| {
        !v.trim().is_empty() && v.len() <= max && !v.chars().any(char::is_control)
    };
    if !safe(name, 64) || !safe(apn, 100) {
        return Err("invalid mobile profile name or APN".into());
    }
    run(
        "nmcli",
        &[
            "connection",
            "add",
            "type",
            "gsm",
            "ifname",
            "*",
            "con-name",
            name,
            "apn",
            apn,
        ],
    )?;
    run(
        "nmcli",
        &[
            "connection",
            "modify",
            name,
            "gsm.pin-flags",
            "2",
            "gsm.password-flags",
            "2",
        ],
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnterpriseWifiConfiguration {
    pub profile_name: String,
    pub ssid: String,
    pub identity: String,
    pub anonymous_identity: Option<String>,
    pub ca_certificate: String,
    pub method: EnterpriseEapMethod,
    pub phase2_auth: Option<String>,
    pub client_certificate: Option<String>,
    pub private_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificateInfo {
    pub subject: String,
    pub expires: String,
    pub sha256_fingerprint: String,
}

/// # Errors
///
/// Returns an error message if `path` is not an existing absolute regular file, is larger than
/// 1 MiB, cannot be parsed as one PEM/DER certificate, or the certificate has expired.
pub fn inspect_certificate(path: &str) -> Result<CertificateInfo, String> {
    let p = std::path::Path::new(path);
    if !p.is_absolute() || !p.is_file() {
        return Err("certificate must be an existing absolute file".into());
    }
    let metadata =
        std::fs::metadata(p).map_err(|error| format!("cannot read certificate: {error}"))?;
    if metadata.len() > 1024 * 1024 {
        return Err("certificate must be no larger than 1 MiB".into());
    }
    let bytes = std::fs::read(p).map_err(|error| format!("cannot read certificate: {error}"))?;
    let info = tilo_crypto::inspect_certificate(&bytes)?;
    Ok(CertificateInfo {
        subject: info.subject,
        expires: info.expires,
        sha256_fingerprint: info.sha256_fingerprint,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EnterpriseEapMethod {
    #[default]
    Peap,
    Ttls,
    Tls,
}

impl EnterpriseWifiConfiguration {
    fn validate(&self) -> Result<(), String> {
        let safe = |value: &str, max: usize| {
            !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        };
        if !safe(&self.profile_name, 64) || !safe(&self.ssid, 32) || !safe(&self.identity, 253) {
            return Err("enterprise Wi-Fi name, SSID, or identity is invalid".into());
        }
        let ca = std::path::Path::new(&self.ca_certificate);
        if !ca.is_absolute() || !ca.is_file() {
            return Err("choose an existing absolute CA certificate file".into());
        }
        if self.method == EnterpriseEapMethod::Tls {
            for (label, path) in [
                ("client certificate", self.client_certificate.as_deref()),
                ("private key", self.private_key.as_deref()),
            ] {
                if !path.is_some_and(|p| {
                    let p = std::path::Path::new(p);
                    p.is_absolute() && p.is_file()
                }) {
                    return Err(format!("choose an existing absolute {label} file"));
                }
            }
        } else if !matches!(
            self.phase2_auth.as_deref().unwrap_or("mschapv2"),
            "mschapv2" | "pap" | "chap"
        ) {
            return Err("unsupported enterprise inner-authentication method".into());
        }
        if self
            .anonymous_identity
            .as_deref()
            .is_some_and(|value| !safe(value, 253))
        {
            return Err("anonymous identity is invalid".into());
        }
        Ok(())
    }
}

/// Configure the broadly deployed PEAP/MSCHAPv2 enterprise profile using
/// `NetworkManager`'s standard 802.1X setting. A CA certificate is mandatory;
/// Tilo never offers the dangerously convenient "do not validate" mode.
/// The password is fed to `nmcli --ask` on stdin and marked not-saved.
///
/// # Errors
///
/// Returns an error message if `config` fails validation, its certificates cannot be
/// inspected, `password` is empty or invalid for a non-TLS method, or `nmcli` is not installed
/// or a step of creating/modifying/activating the profile fails.
pub fn connect_enterprise_wifi(
    config: &EnterpriseWifiConfiguration,
    password: &str,
) -> Result<(), String> {
    config.validate()?;
    let _ca = inspect_certificate(&config.ca_certificate)?;
    if let Some(client) = config.client_certificate.as_deref() {
        let _ = inspect_certificate(client)?;
    }
    if config.method != EnterpriseEapMethod::Tls
        && (password.is_empty() || password.chars().any(|c| matches!(c, '\n' | '\r')))
    {
        return Err("enterprise Wi-Fi password is empty or invalid".into());
    }
    let existing = run_capture("nmcli", &["-t", "-f", "NAME", "connection", "show"])?;
    if !existing.lines().any(|name| name == config.profile_name) {
        run(
            "nmcli",
            &[
                "connection",
                "add",
                "type",
                "wifi",
                "con-name",
                &config.profile_name,
                "ssid",
                &config.ssid,
            ],
        )?;
    }
    let anonymous = config.anonymous_identity.as_deref().unwrap_or("");
    let method = match config.method {
        EnterpriseEapMethod::Peap => "peap",
        EnterpriseEapMethod::Ttls => "ttls",
        EnterpriseEapMethod::Tls => "tls",
    };
    let phase2 = if config.method == EnterpriseEapMethod::Tls {
        ""
    } else {
        config.phase2_auth.as_deref().unwrap_or("mschapv2")
    };
    run(
        "nmcli",
        &[
            "connection",
            "modify",
            &config.profile_name,
            "wifi-sec.key-mgmt",
            "wpa-eap",
            "802-1x.eap",
            method,
            "802-1x.identity",
            &config.identity,
            "802-1x.anonymous-identity",
            anonymous,
            "802-1x.phase2-auth",
            phase2,
            "802-1x.ca-cert",
            &config.ca_certificate,
            "802-1x.client-cert",
            config.client_certificate.as_deref().unwrap_or(""),
            "802-1x.private-key",
            config.private_key.as_deref().unwrap_or(""),
            "802-1x.password-flags",
            "2",
            "802-1x.private-key-password-flags",
            "2",
        ],
    )?;
    let input = format!("{password}\n");
    crate::process::run_with_input(
        "nmcli",
        &[
            "--ask",
            "--wait",
            "45",
            "connection",
            "up",
            &config.profile_name,
        ],
        input.as_bytes(),
        std::time::Duration::from_secs(50),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nmcli_profiles_radios_and_terse_fields_parse_exactly() {
        // Phase `parses_editable_networkmanager_profile_values`.
        {
            let parsed = parse_connection_profile_settings(concat!(
                "connection.metered:yes\n",
                "802-11-wireless.cloned-mac-address:stable-ssid\n",
                "proxy.method:auto\n",
                "proxy.pac-url:https\\://proxy.example/pac\n",
                "proxy.browser-only:yes\n",
                "ipv4.method:manual\n",
                "ipv4.addresses:192.0.2.10/24\n",
                "ipv4.gateway:192.0.2.1\n",
                "ipv4.dns:1.1.1.1,9.9.9.9\n",
            ))
            .unwrap();
            assert_eq!(parsed.metered, Some(MeteredPolicy::Metered));
            assert_eq!(parsed.mac, Some(MacPolicy::Stable));
            assert_eq!(
                parsed.proxy.unwrap().automatic_url.as_deref(),
                Some("https://proxy.example/pac")
            );
            assert_eq!(parsed.ipv4.unwrap().addresses, ["192.0.2.10/24"]);
        }
        // Phase `preserves_unrecognized_profile_properties`.
        {
            let parsed = parse_connection_profile_settings(concat!(
                "connection.metered:vendor-value\n",
                "802-11-wireless.cloned-mac-address:02\\:00\\:00\\:00\\:00\\:01\n",
                "proxy.method:manual\n",
                "ipv4.method:shared\n",
            ))
            .unwrap();
            assert_eq!(parsed, ConnectionProfileSettings::default());
        }
        // Phase `parse_nmcli_radio_states`.
        {
            assert!(parse_nmcli_radio("enabled\n").unwrap());
            assert!(!parse_nmcli_radio("disabled\n").unwrap());
            assert!(parse_nmcli_radio("garbled").is_err());
        }
        // Phase `split_terse_handles_escaped_colons`.
        {
            let line = r"*:Home\:Network:75:WPA2";
            let fields = split_terse(line);
            assert_eq!(fields, vec!["*", "Home:Network", "75", "WPA2"]);
        }
        // Phase `signal_bucket_maps_quartiles`.
        {
            assert_eq!(signal_pct_to_bucket(0), 1);
            assert_eq!(signal_pct_to_bucket(20), 1);
            assert_eq!(signal_pct_to_bucket(35), 2);
            assert_eq!(signal_pct_to_bucket(60), 3);
            assert_eq!(signal_pct_to_bucket(95), 4);
        }
        // Phase `wifi_read_is_unavailable_without_an_adapter`.
        {
            let radios = RadioState::default();
            assert_eq!(
                wifi_from_radios(&radios).unwrap_err(),
                "no Wi-Fi adapter on this machine"
            );
            let on = RadioState {
                wifi_available: true,
                wifi_enabled: true,
                ..RadioState::default()
            };
            assert!(wifi_from_radios(&on).unwrap());
            let off = RadioState {
                wifi_available: true,
                wifi_enabled: false,
                ..RadioState::default()
            };
            assert!(!wifi_from_radios(&off).unwrap());
        }
    }

    #[test]
    fn wifi_lists_dedupe_ssids_and_keep_security_modes() {
        // Phase `parse_nmcli_wifi_list_basic`.
        {
            let stdout = "\
    *:Home_Network:80:WPA2\n\
    :Cafe_Guest:55:--\n\
    :Neighbour_5G:30:WPA2\n\
    ";
            let nets = parse_nmcli_wifi_list(stdout);
            assert_eq!(nets.len(), 3);
            assert_eq!(nets[0].ssid, "Home_Network");
            assert!(nets[0].connected);
            assert!(nets[0].secure);
            assert_eq!(nets[0].security, WifiSecurity::Wpa2);
            assert_eq!(nets[0].signal, 4);
            assert!(!nets[1].connected);
            assert!(!nets[1].secure);
            assert_eq!(nets[1].security, WifiSecurity::Open);
            assert_eq!(nets[1].signal, 3);
            assert_eq!(nets[2].signal, 2);
        }
        // Phase `wifi_security_preserves_supported_authentication_modes`.
        {
            for (nmcli, expected) in [
                ("--", WifiSecurity::Open),
                ("WPA2", WifiSecurity::Wpa2),
                ("WPA3 SAE", WifiSecurity::Wpa3),
                ("OWE", WifiSecurity::Owe),
                ("WPA2 802.1X", WifiSecurity::Enterprise),
                ("WEP", WifiSecurity::Unknown),
            ] {
                assert_eq!(WifiSecurity::from_nmcli(nmcli), expected, "{nmcli}");
            }
            assert!(WifiSecurity::Wpa2.supports_portable_passphrase());
            assert!(WifiSecurity::Wpa3.supports_portable_passphrase());
            assert!(!WifiSecurity::Enterprise.supports_portable_passphrase());
            assert!(!WifiSecurity::Owe.supports_portable_passphrase());
        }
        // Phase `connected_bssid_supplies_security_when_ssids_are_deduplicated`.
        {
            let nets = parse_nmcli_wifi_list(":Transition:90:WPA2\n*:Transition:40:WPA3 SAE\n");
            assert_eq!(nets.len(), 1);
            assert_eq!(nets[0].security, WifiSecurity::Wpa3);
            assert!(nets[0].connected);
            assert_eq!(nets[0].signal, 4);
        }
        // Phase `parse_nmcli_wifi_list_dedupes_ssids`.
        {
            let stdout = "\
    :DualBand:30:WPA2\n\
    *:DualBand:75:WPA2\n\
    ";
            let nets = parse_nmcli_wifi_list(stdout);
            assert_eq!(nets.len(), 1);
            assert_eq!(nets[0].signal, 4);
            assert!(nets[0].connected);
        }
        // Phase `parse_nmcli_wifi_list_skips_empty_ssid`.
        {
            let stdout = ":::WPA2\n*:Real:80:WPA2\n";
            let nets = parse_nmcli_wifi_list(stdout);
            assert_eq!(nets.len(), 1);
            assert_eq!(nets[0].ssid, "Real");
        }
    }
}

/// Wired (ethernet) connection snapshot for the Settings network panel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WiredInfo {
    /// `true` when a wired device is present and in the `connected` state.
    pub connected: bool,
    /// The primary wired device name (e.g. `eth0`, `enp3s0`), if any.
    pub device: Option<String>,
    /// Negotiated link speed as reported by nmcli (e.g. `1000 Mb/s`).
    pub speed: Option<String>,
    /// The device's first IPv4 address (no CIDR suffix).
    pub ipv4: Option<String>,
}

/// Snapshot the primary wired (ethernet) device via `nmcli`. Returns a default
/// (`connected: false`) `WiredInfo` when no ethernet device is present, so the Settings
/// panel renders an "unplugged" card without an error path.
///
/// # Errors
///
/// Returns an error message if `nmcli` is not installed or the device-status query fails.
pub fn get_wired() -> Result<WiredInfo, String> {
    let status = run_capture(
        "nmcli",
        &["-t", "-f", "DEVICE,TYPE,STATE", "device", "status"],
    )?;
    let Some((device, connected)) = parse_nmcli_wired_status(&status) else {
        return Ok(WiredInfo::default());
    };
    let mut info = WiredInfo {
        connected,
        device: Some(device.clone()),
        speed: None,
        ipv4: None,
    };
    if let Ok(show) = run_capture(
        "nmcli",
        &[
            "-t",
            "-f",
            "CAPABILITIES.SPEED,IP4.ADDRESS",
            "device",
            "show",
            &device,
        ],
    ) {
        let (speed, ipv4) = parse_nmcli_wired_show(&show);
        info.speed = speed;
        info.ipv4 = ipv4;
    }
    Ok(info)
}

/// Connect or disconnect the primary Ethernet interface.  This is scoped to
/// one device; unlike `nmcli networking off`, it cannot accidentally disable
/// Wi-Fi, VPN, or the entire `NetworkManager` service.
///
/// # Errors
///
/// Returns an error message if `nmcli` is not installed, no wired interface is found, or the
/// connect/disconnect call fails.
pub fn set_wired(enabled: bool) -> Result<(), String> {
    let status = run_capture(
        "nmcli",
        &["-t", "-f", "DEVICE,TYPE,STATE", "device", "status"],
    )?;
    let (device, connected) = parse_nmcli_wired_status(&status)
        .ok_or_else(|| "no wired network interface found".to_string())?;
    if connected == enabled {
        return Ok(());
    }
    let verb = if enabled { "connect" } else { "disconnect" };
    run("nmcli", &["device", verb, &device])
}

/// First ethernet device + connected flag, from terse `nmcli -t -f DEVICE,TYPE,STATE device
/// status` (`DEVICE:TYPE:STATE`).
fn parse_nmcli_wired_status(stdout: &str) -> Option<(String, bool)> {
    for line in stdout.lines() {
        let mut it = line.splitn(3, ':');
        let device = it.next().unwrap_or("").trim();
        let ty = it.next().unwrap_or("").trim();
        let state = it.next().unwrap_or("").trim();
        if ty == "ethernet" && !device.is_empty() {
            return Some((device.to_string(), state == "connected"));
        }
    }
    None
}

/// Link speed + first IPv4 from terse `nmcli -t -f CAPABILITIES.SPEED,IP4.ADDRESS device
/// show <dev>` (`FIELD:value`, with `IP4.ADDRESS[1]:1.2.3.4/24`).
fn parse_nmcli_wired_show(stdout: &str) -> (Option<String>, Option<String>) {
    let mut speed = None;
    let mut ipv4 = None;
    for line in stdout.lines() {
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if field == "CAPABILITIES.SPEED" {
            if !value.is_empty() && value != "unknown" {
                speed = Some(value.to_string());
            }
        } else if field.starts_with("IP4.ADDRESS") && ipv4.is_none() {
            let addr = value.split('/').next().unwrap_or("").trim();
            if !addr.is_empty() {
                ipv4 = Some(addr.to_string());
            }
        }
    }
    (speed, ipv4)
}

#[cfg(test)]
mod wired_tests {
    use super::*;

    #[test]
    fn wired_status_and_show_output_parse_devices_speed_and_address() {
        for (case, out, want) in [
            (
                "parses_connected_ethernet_from_status",
                "lo:loopback:unmanaged\neth0:ethernet:connected\nwlan0:wifi:connected\n",
                Some(("eth0".to_string(), true)),
            ),
            (
                "disconnected_ethernet_reports_device_but_not_connected",
                "enp3s0:ethernet:disconnected\n",
                Some(("enp3s0".to_string(), false)),
            ),
            ("no_ethernet_device_is_none", "wlan0:wifi:connected\n", None),
        ] {
            assert_eq!(parse_nmcli_wired_status(out), want, "case {case}");
        }
        for (case, out, want_speed, want_ipv4) in [
            (
                "parses_speed_and_first_ipv4_from_show",
                "CAPABILITIES.SPEED:1000 Mb/s\nIP4.ADDRESS[1]:192.168.1.42/24\nIP4.ADDRESS[2]:10.0.0.9/8\n",
                Some("1000 Mb/s"),
                Some("192.168.1.42"),
            ),
            (
                "unknown_speed_and_missing_ip_are_none",
                "CAPABILITIES.SPEED:unknown\n",
                None,
                None,
            ),
        ] {
            let (speed, ipv4) = parse_nmcli_wired_show(out);
            assert_eq!(speed.as_deref(), want_speed, "case {case}: speed");
            assert_eq!(ipv4.as_deref(), want_ipv4, "case {case}: ipv4");
        }
    }
}

#[cfg(test)]
mod portable_profile_tests {
    use super::*;

    #[test]
    fn portable_profiles_keep_only_personal_wifi_secrets() {
        // Phase `portable_profile_keeps_only_ssid_mode_and_secret`.
        {
            let profile = parse_portable_wifi_profile(
                "802-11-wireless\nHome network\nwpa-psk\ncorrect horse battery staple\n",
            )
            .unwrap();
            assert_eq!(profile.ssid, "Home network");
            assert_eq!(profile.security, WifiSecurity::Wpa2);
            assert_eq!(profile.password.as_str(), "correct horse battery staple");
        }
        // Phase `enterprise_and_non_wifi_profiles_fail_closed`.
        {
            assert!(
                parse_portable_wifi_profile("802-11-wireless\nWork\nwpa-eap\nsecret\n").is_err()
            );
            assert!(parse_portable_wifi_profile("802-3-ethernet\nLAN\nwpa-psk\nsecret\n").is_err());
        }
    }
}

#[cfg(test)]
mod link_status_tests {
    use super::*;

    /// `nmcli -t -f TYPE,STATE,CONNECTION,DEVICE device status`.
    const BOTH_UP: &str = "ethernet:connected:Wired connection 1:enp0s31f6\n\
wifi:connected:Cafe WiFi:wlp2s0\n\
loopback:unmanaged::lo\n";

    #[test]
    fn device_status_prefers_wired_and_reports_only_connected_links() {
        // Phase `wired_wins_when_both_links_are_up`.
        {
            let s = parse_nmcli_device_status(BOTH_UP);
            assert_eq!(s.kind, LinkKind::Wired);
            assert_eq!(s.name.as_deref(), Some("Wired connection 1"));
            assert_eq!(s.signal, 0, "a cable has no signal strength to report");
        }
        // Phase `wifi_reports_its_ssid_when_there_is_no_cable`.
        {
            let s = parse_nmcli_device_status(
                "wifi:connected:Cafe WiFi:wlp2s0\nloopback:unmanaged::lo\n",
            );
            assert_eq!(s.kind, LinkKind::Wifi);
            assert_eq!(s.name.as_deref(), Some("Cafe WiFi"));
            assert!(s.signal > 0, "a connected radio never reads as no-signal");
        }
        // Phase `a_link_still_negotiating_is_not_online`.
        {
            let out = "wifi:connecting (getting IP configuration):Cafe WiFi:wlp2s0\n";
            let s = parse_nmcli_device_status(out);
            assert_eq!(
                s.kind,
                LinkKind::Offline,
                "a half-associated link carries no traffic and must not read as connected"
            );
            assert!(!s.is_online());
        }
        // Phase `nothing_connected_is_offline`.
        {
            let s = parse_nmcli_device_status(
                "wifi:disconnected::wlp2s0\nethernet:unavailable::eth0\n",
            );
            assert_eq!(s, LinkStatus::offline());
        }
        // Phase `an_ssid_containing_a_colon_survives_the_terse_split`.
        {
            let s = parse_nmcli_device_status(r"wifi:connected:Home\:Network:wlp2s0");
            assert_eq!(s.name.as_deref(), Some("Home:Network"));
        }
    }

    /// Real `/proc/net/wireless`, wext scale (quality out of 70).
    const PROC_WIRELESS: &str = "Inter-| sta-|   Quality        |   Discarded packets\n\
 face | tus | link level noise |  nwid  crypt   frag  retry   misc\n\
wlp2s0: 0000   49.  -61.  -256        0      0      0     55      0\n";

    #[test]
    fn proc_net_wireless_quality_is_scaled_to_a_percentage() {
        // A driver reporting past the wext ceiling is already a percentage.
        let driver_percent =
            "hdr\nhdr\nwlan0: 0000   88.  -40.  -256        0      0      0      0\n";
        for (case, raw, want) in [
            // 49/70 = 70%.
            (
                "link_quality_is_scaled_off_the_wext_ceiling",
                PROC_WIRELESS,
                Some(70),
            ),
            (
                "a_driver_reporting_percent_directly_is_not_rescaled",
                driver_percent,
                Some(88),
            ),
            (
                "no_wireless_interfaces_yields_no_reading",
                "Inter-| sta-|   Quality\n face | tus | link\n",
                None,
            ),
        ] {
            assert_eq!(parse_proc_net_wireless(raw), want, "case {case}");
        }
        // 70% is bucket 3 on the shared signal scale.
        assert_eq!(signal_pct_to_bucket(70), 3);
    }
}
