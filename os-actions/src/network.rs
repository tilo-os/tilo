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

/// Snapshot of the wired (Ethernet) connection, read from
/// `nmcli`. `None` for [`WiredInfo::ipv4`] / `speed` when the field
/// isn't reported (link down, or nmcli not present).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WiredInfo {
    /// `true` when an Ethernet device is connected.
    pub connected: bool,
    /// The device name (`eth0`, `enp3s0`, …) when one was found.
    pub device: Option<String>,
    /// The negotiated link speed text (e.g. `"1000 Mb/s"`), if known.
    pub speed: Option<String>,
    /// The device's primary IPv4 address (without the CIDR suffix).
    pub ipv4: Option<String>,
}

/// Read the state of the first wired (Ethernet) device via
/// `nmcli -t -f DEVICE,TYPE,STATE,CONNECTION device status` plus a
/// per-device `nmcli -t -f IP4.ADDRESS,GENERAL.SPEED device show`.
/// Best-effort: any missing field degrades to `None` rather than
/// erroring, so the pane always has something to show.
pub fn get_wired() -> Result<WiredInfo, String> {
    let out = run_capture(
        "nmcli",
        &["-t", "-f", "DEVICE,TYPE,STATE", "device", "status"],
    )?;
    let device = parse_first_ethernet_device(&out);
    let Some((dev, connected)) = device else {
        return Ok(WiredInfo::default());
    };
    let mut info = WiredInfo {
        connected,
        device: Some(dev.clone()),
        speed: None,
        ipv4: None,
    };
    if connected {
        if let Ok(detail) = run_capture(
            "nmcli",
            &["-t", "-f", "IP4.ADDRESS,GENERAL.SPEED", "device", "show", &dev],
        ) {
            let (ipv4, speed) = parse_device_detail(&detail);
            info.ipv4 = ipv4;
            info.speed = speed;
        }
    }
    Ok(info)
}

/// Find the first `ethernet`-type device in `nmcli device status` terse
/// output, returning `(device, connected)`. `connected` is true when the
/// STATE column starts with `connected`.
fn parse_first_ethernet_device(stdout: &str) -> Option<(String, bool)> {
    for line in stdout.lines() {
        let fields = split_terse(line);
        if fields.len() < 3 {
            continue;
        }
        let dev = fields[0].trim();
        let ty = fields[1].trim();
        let state = fields[2].trim();
        if ty == "ethernet" && !dev.is_empty() {
            // STATE is e.g. "connected", "disconnected", "unavailable".
            let connected = state.starts_with("connected");
            return Some((dev.to_string(), connected));
        }
    }
    None
}

/// Parse the IPv4 address (stripping the `/prefix`) and link speed from
/// `nmcli -t -f IP4.ADDRESS,GENERAL.SPEED device show <dev>`.
fn parse_device_detail(stdout: &str) -> (Option<String>, Option<String>) {
    let mut ipv4 = None;
    let mut speed = None;
    for line in stdout.lines() {
        if let Some(rest) = line.split_once(':') {
            let (key, val) = (rest.0.trim(), rest.1.trim());
            if key.starts_with("IP4.ADDRESS") && ipv4.is_none() && !val.is_empty() {
                // Strip the CIDR suffix: "192.168.1.42/24" -> "192.168.1.42".
                ipv4 = Some(val.split('/').next().unwrap_or(val).to_string());
            } else if key == "GENERAL.SPEED" && !val.is_empty() && val != "unknown" {
                // nmcli reports a bare number (Mbit/s); annotate it.
                speed = Some(if val.chars().all(|c| c.is_ascii_digit()) {
                    format!("{val} Mb/s")
                } else {
                    val.to_string()
                });
            }
        }
    }
    (ipv4, speed)
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

    #[test]
    fn parse_first_ethernet_device_picks_connected_ethernet() {
        let stdout = "\
lo:loopback:unmanaged\n\
wlan0:wifi:connected\n\
enp3s0:ethernet:connected\n\
";
        let got = parse_first_ethernet_device(stdout);
        assert_eq!(got, Some(("enp3s0".to_string(), true)));
    }

    #[test]
    fn parse_first_ethernet_device_reports_disconnected() {
        let stdout = "eth0:ethernet:disconnected\n";
        let got = parse_first_ethernet_device(stdout);
        assert_eq!(got, Some(("eth0".to_string(), false)));
    }

    #[test]
    fn parse_first_ethernet_device_none_when_absent() {
        let stdout = "wlan0:wifi:connected\nlo:loopback:unmanaged\n";
        assert_eq!(parse_first_ethernet_device(stdout), None);
    }

    #[test]
    fn parse_device_detail_extracts_ip_and_speed() {
        let stdout = "\
IP4.ADDRESS[1]:192.168.1.42/24\n\
GENERAL.SPEED:1000\n\
";
        let (ip, speed) = parse_device_detail(stdout);
        assert_eq!(ip.as_deref(), Some("192.168.1.42"));
        assert_eq!(speed.as_deref(), Some("1000 Mb/s"));
    }

    #[test]
    fn parse_device_detail_handles_unknown_speed() {
        let stdout = "IP4.ADDRESS[1]:10.0.0.5/8\nGENERAL.SPEED:unknown\n";
        let (ip, speed) = parse_device_detail(stdout);
        assert_eq!(ip.as_deref(), Some("10.0.0.5"));
        assert_eq!(speed, None);
    }
}
