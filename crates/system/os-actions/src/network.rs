//! Wi-Fi control via `nmcli`.

use crate::process::{run, run_capture};

/// One Wi-Fi network entry returned by [`list_wifi_networks`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetworkInfo {
    pub ssid: String,
    /// Signal-strength bucket in `[0, 4]`. Mapped from the nmcli percentage via
    /// [`signal_pct_to_bucket`].
    pub signal: u8,
    /// `true` when the row's SECURITY column is non-empty (i.e. not `"--"`).
    pub secure: bool,
    /// `true` when the row's IN-USE column is `*`.
    pub connected: bool,
}

/// Toggle the Wi-Fi radio on or off via `nmcli radio wifi on|off`.
pub fn set_wifi(enabled: bool) -> Result<(), String> {
    let state = if enabled { "on" } else { "off" };
    run("nmcli", &["radio", "wifi", state])
}

/// Read the Wi-Fi radio state. `nmcli radio wifi` prints `enabled` or `disabled` on stdout.
pub fn get_wifi() -> Result<bool, String> {
    let out = run_capture("nmcli", &["radio", "wifi"])?;
    parse_nmcli_radio(&out)
}

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
        if fields.len() < 4 {
            continue;
        }
        let in_use = fields[0].trim();
        let ssid = fields[1].trim().to_string();
        if ssid.is_empty() {
            continue;
        }
        let signal_pct: u8 = fields[2].trim().parse().unwrap_or(0);
        let security = fields[3].trim();
        out.push(WifiNetworkInfo {
            ssid,
            signal: signal_pct_to_bucket(signal_pct),
            secure: !security.is_empty() && security != "--",
            connected: in_use == "*",
        });
    }
    // De-duplicate SSIDs (multiple BSSIDs broadcasting the same SSID show up as separate rows):
    // keep the strongest signal and preserve the connected flag from any row.
    let mut deduped: Vec<WifiNetworkInfo> = Vec::new();
    for entry in out {
        if let Some(existing) = deduped.iter_mut().find(|e| e.ssid == entry.ssid) {
            if entry.signal > existing.signal {
                existing.signal = entry.signal;
            }
            if entry.connected {
                existing.connected = true;
            }
            if entry.secure {
                existing.secure = true;
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
        if f.len() < 3 {
            continue;
        }
        let (kind_s, state, conn) = (f[0].trim(), f[1].trim(), f[2].trim());
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
        let pct = if q <= 70.0 {
            ((q / 70.0) * 100.0).round().clamp(0.0, 100.0) as u8
        } else {
            q.round().clamp(0.0, 100.0) as u8
        };
        best = Some(best.map_or(pct, |b: u8| b.max(pct)));
    }
    best
}

/// Connect to a Wi-Fi SSID. `Some(pw)` appends a `password <pw>` clause; `None` omits it
/// (open networks or already-known SSIDs). Synchronous: nmcli blocks until association
/// completes or fails.
pub fn connect_wifi(ssid: &str, password: Option<&str>) -> Result<(), String> {
    let mut args: Vec<&str> = vec!["device", "wifi", "connect", ssid];
    if let Some(pw) = password {
        args.push("password");
        args.push(pw);
    }
    run("nmcli", &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_nmcli_radio_states() {
        assert!(parse_nmcli_radio("enabled\n").unwrap());
        assert!(!parse_nmcli_radio("disabled\n").unwrap());
        assert!(parse_nmcli_radio("garbled").is_err());
    }

    #[test]
    fn split_terse_handles_escaped_colons() {
        let line = r"*:Home\:Network:75:WPA2";
        let fields = split_terse(line);
        assert_eq!(fields, vec!["*", "Home:Network", "75", "WPA2"]);
    }

    #[test]
    fn signal_bucket_maps_quartiles() {
        assert_eq!(signal_pct_to_bucket(0), 1);
        assert_eq!(signal_pct_to_bucket(20), 1);
        assert_eq!(signal_pct_to_bucket(35), 2);
        assert_eq!(signal_pct_to_bucket(60), 3);
        assert_eq!(signal_pct_to_bucket(95), 4);
    }

    #[test]
    fn parse_nmcli_wifi_list_basic() {
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
        assert_eq!(nets[0].signal, 4);
        assert!(!nets[1].connected);
        assert!(!nets[1].secure);
        assert_eq!(nets[1].signal, 3);
        assert_eq!(nets[2].signal, 2);
    }

    #[test]
    fn parse_nmcli_wifi_list_dedupes_ssids() {
        let stdout = "\
:DualBand:30:WPA2\n\
*:DualBand:75:WPA2\n\
";
        let nets = parse_nmcli_wifi_list(stdout);
        assert_eq!(nets.len(), 1);
        assert_eq!(nets[0].signal, 4);
        assert!(nets[0].connected);
    }

    #[test]
    fn parse_nmcli_wifi_list_skips_empty_ssid() {
        let stdout = ":::WPA2\n*:Real:80:WPA2\n";
        let nets = parse_nmcli_wifi_list(stdout);
        assert_eq!(nets.len(), 1);
        assert_eq!(nets[0].ssid, "Real");
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
    fn parses_connected_ethernet_from_status() {
        let out = "lo:loopback:unmanaged\neth0:ethernet:connected\nwlan0:wifi:connected\n";
        assert_eq!(
            parse_nmcli_wired_status(out),
            Some(("eth0".to_string(), true))
        );
    }

    #[test]
    fn disconnected_ethernet_reports_device_but_not_connected() {
        let out = "enp3s0:ethernet:disconnected\n";
        assert_eq!(
            parse_nmcli_wired_status(out),
            Some(("enp3s0".to_string(), false))
        );
    }

    #[test]
    fn no_ethernet_device_is_none() {
        assert_eq!(parse_nmcli_wired_status("wlan0:wifi:connected\n"), None);
    }

    #[test]
    fn parses_speed_and_first_ipv4_from_show() {
        let out = "CAPABILITIES.SPEED:1000 Mb/s\nIP4.ADDRESS[1]:192.168.1.42/24\nIP4.ADDRESS[2]:10.0.0.9/8\n";
        let (speed, ipv4) = parse_nmcli_wired_show(out);
        assert_eq!(speed.as_deref(), Some("1000 Mb/s"));
        assert_eq!(ipv4.as_deref(), Some("192.168.1.42"));
    }

    #[test]
    fn unknown_speed_and_missing_ip_are_none() {
        let (speed, ipv4) = parse_nmcli_wired_show("CAPABILITIES.SPEED:unknown\n");
        assert!(speed.is_none());
        assert!(ipv4.is_none());
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
    fn wired_wins_when_both_links_are_up() {
        let s = parse_nmcli_device_status(BOTH_UP);
        assert_eq!(s.kind, LinkKind::Wired);
        assert_eq!(s.name.as_deref(), Some("Wired connection 1"));
        assert_eq!(s.signal, 0, "a cable has no signal strength to report");
    }

    #[test]
    fn wifi_reports_its_ssid_when_there_is_no_cable() {
        let s =
            parse_nmcli_device_status("wifi:connected:Cafe WiFi:wlp2s0\nloopback:unmanaged::lo\n");
        assert_eq!(s.kind, LinkKind::Wifi);
        assert_eq!(s.name.as_deref(), Some("Cafe WiFi"));
        assert!(s.signal > 0, "a connected radio never reads as no-signal");
    }

    #[test]
    fn a_link_still_negotiating_is_not_online() {
        let out = "wifi:connecting (getting IP configuration):Cafe WiFi:wlp2s0\n";
        let s = parse_nmcli_device_status(out);
        assert_eq!(
            s.kind,
            LinkKind::Offline,
            "a half-associated link carries no traffic and must not read as connected"
        );
        assert!(!s.is_online());
    }

    #[test]
    fn nothing_connected_is_offline() {
        let s =
            parse_nmcli_device_status("wifi:disconnected::wlp2s0\nethernet:unavailable::eth0\n");
        assert_eq!(s, LinkStatus::offline());
    }

    #[test]
    fn an_ssid_containing_a_colon_survives_the_terse_split() {
        let s = parse_nmcli_device_status(r"wifi:connected:Home\:Network:wlp2s0");
        assert_eq!(s.name.as_deref(), Some("Home:Network"));
    }

    /// Real `/proc/net/wireless`, wext scale (quality out of 70).
    const PROC_WIRELESS: &str = "Inter-| sta-|   Quality        |   Discarded packets\n\
 face | tus | link level noise |  nwid  crypt   frag  retry   misc\n\
wlp2s0: 0000   49.  -61.  -256        0      0      0     55      0\n";

    #[test]
    fn link_quality_is_scaled_off_the_wext_ceiling() {
        // 49/70 = 70%, which is bucket 3 on the shared signal scale.
        assert_eq!(parse_proc_net_wireless(PROC_WIRELESS), Some(70));
        assert_eq!(signal_pct_to_bucket(70), 3);
    }

    #[test]
    fn a_driver_reporting_percent_directly_is_not_rescaled() {
        let raw = "hdr\nhdr\nwlan0: 0000   88.  -40.  -256        0      0      0      0\n";
        assert_eq!(
            parse_proc_net_wireless(raw),
            Some(88),
            "a value past the wext ceiling is already a percentage"
        );
    }

    #[test]
    fn no_wireless_interfaces_yields_no_reading() {
        let raw = "Inter-| sta-|   Quality\n face | tus | link\n";
        assert_eq!(parse_proc_net_wireless(raw), None);
    }
}
