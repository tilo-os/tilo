//! Wi-Fi control via `nmcli`.

use crate::process::{run, run_capture};

/// One Wi-Fi network entry returned by [`list_wifi_networks`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetworkInfo {
    pub ssid: String,
    /// Signal-strength bucket in `[0, 4]`. Mapped from the nmcli
    /// percentage via [`signal_pct_to_bucket`].
    pub signal: u8,
    /// `true` when the row's SECURITY column is non-empty (i.e. not
    /// `"--"`).
    pub secure: bool,
    /// `true` when the row's IN-USE column is `*`.
    pub connected: bool,
}

/// Toggle the Wi-Fi radio on or off via `nmcli radio wifi on|off`.
pub fn set_wifi(enabled: bool) -> Result<(), String> {
    let state = if enabled { "on" } else { "off" };
    run("nmcli", &["radio", "wifi", state])
}

/// Read the Wi-Fi radio state. `nmcli radio wifi` prints `enabled` or
/// `disabled` on stdout.
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

/// Scan + list visible Wi-Fi networks via
/// `nmcli -t -f IN-USE,SSID,SIGNAL,SECURITY device wifi list`. The `-t`
/// (terse) flag prints one row per line, fields separated by `:`.
pub fn list_wifi_networks() -> Result<Vec<WifiNetworkInfo>, String> {
    let out = run_capture(
        "nmcli",
        &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "device", "wifi", "list"],
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
    // De-duplicate SSIDs (multiple BSSIDs broadcasting the same SSID
    // show up as separate rows in the terse output). Keep the strongest
    // signal and preserve the connected flag from any row.
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

/// Split a single nmcli `-t` (terse) line on unescaped `:` separators.
/// `\:` inside a field stays a literal colon in the field value.
fn split_terse(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                buf.push(next);
                chars.next();
                continue;
            }
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

/// Map nmcli's `SIGNAL` percentage `0..=100` onto the popover's signal
/// bucket `0..=4`.
fn signal_pct_to_bucket(pct: u8) -> u8 {
    match pct {
        0..=20 => 1,
        21..=45 => 2,
        46..=70 => 3,
        _ => 4,
    }
}

/// Connect to a Wi-Fi SSID. Passing `Some(pw)` appends a
/// `password <pw>` clause; `None` omits it (open networks or already-
/// known SSIDs). The call is synchronous: nmcli blocks until
/// association either completes or fails.
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
        assert_eq!(parse_nmcli_radio("enabled\n").unwrap(), true);
        assert_eq!(parse_nmcli_radio("disabled\n").unwrap(), false);
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

/// Snapshot the primary wired (ethernet) device via `nmcli`. Returns a
/// default (`connected: false`) `WiredInfo` when no ethernet device is
/// present, so the Settings panel renders an "unplugged" card without an
/// error path.
pub fn get_wired() -> Result<WiredInfo, String> {
    let status = run_capture("nmcli", &["-t", "-f", "DEVICE,TYPE,STATE", "device", "status"])?;
    let Some((device, connected)) = parse_nmcli_wired_status(&status) else {
        return Ok(WiredInfo::default());
    };
    let mut info = WiredInfo { connected, device: Some(device.clone()), speed: None, ipv4: None };
    if let Ok(show) = run_capture(
        "nmcli",
        &["-t", "-f", "CAPABILITIES.SPEED,IP4.ADDRESS", "device", "show", &device],
    ) {
        let (speed, ipv4) = parse_nmcli_wired_show(&show);
        info.speed = speed;
        info.ipv4 = ipv4;
    }
    Ok(info)
}

/// First ethernet device + connected flag, from terse
/// `nmcli -t -f DEVICE,TYPE,STATE device status` (`DEVICE:TYPE:STATE`).
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

/// Link speed + first IPv4 from terse
/// `nmcli -t -f CAPABILITIES.SPEED,IP4.ADDRESS device show <dev>`
/// (`FIELD:value`, with `IP4.ADDRESS[1]:1.2.3.4/24`).
fn parse_nmcli_wired_show(stdout: &str) -> (Option<String>, Option<String>) {
    let mut speed = None;
    let mut ipv4 = None;
    for line in stdout.lines() {
        let Some((field, value)) = line.split_once(':') else { continue };
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
        assert_eq!(parse_nmcli_wired_status(out), Some(("eth0".to_string(), true)));
    }

    #[test]
    fn disconnected_ethernet_reports_device_but_not_connected() {
        let out = "enp3s0:ethernet:disconnected\n";
        assert_eq!(parse_nmcli_wired_status(out), Some(("enp3s0".to_string(), false)));
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
