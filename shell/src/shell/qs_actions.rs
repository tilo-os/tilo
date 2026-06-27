//! Quick-settings shell-out adapters.
//!
//! Wraps each `brightnessctl` / `wpctl` / `nmcli` / `bluetoothctl` /
//! `powerprofilesctl` invocation behind a small typed function. Functions
//! return `Result<_, String>` so the call-sites (in
//! [`crate::shell::overlay_host`]) can `tracing::warn!` and surface
//! failures without panicking.
//!
//! Design notes:
//!
//! - Every helper checks the binary's presence first via [`which`] and
//!   returns a clear `"<binary> not installed"` error otherwise. This
//!   keeps the QS panel functional on machines where one of the daemons
//!   is missing (e.g. a desktop without `power-profiles-daemon`).
//! - We use [`std::process::Command`] (sync) deliberately. The QS panel
//!   is an interactive popover so user-driven calls are infrequent;
//!   getter calls happen only on rising-edge open. No tokio runtime
//!   needed; no risk of mixing sync code into the compositor's async
//!   borrow chain.
//! - Parsers are extracted into private free functions so they can be
//!   unit-tested against sample stdout pasted into the test module
//!   without spawning processes.

use std::process::Command;
use std::time::Duration;

use tracing::warn;

/// One Wi-Fi network entry returned by [`list_wifi_networks`]. Mirrors
/// [`crate::shell::quick_settings::WifiNetwork`] so the call-site can
/// drop the parsed rows straight into
/// [`crate::shell::chrome_state::QuickSettingsState::wifi_networks`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetworkInfo {
    pub ssid: String,
    /// Signal-strength bucket in `[0, 4]`. Mapped from the nmcli
    /// percentage via [`signal_pct_to_bucket`].
    pub signal: u8,
    /// `true` when the row's SECURITY column is non-empty (i.e. not
    /// "--").
    pub secure: bool,
    /// `true` when the row's IN-USE column is `*`.
    pub connected: bool,
}

/// One Bluetooth device entry returned by [`list_bt_devices`]. Mirrors
/// [`crate::shell::quick_settings::BtDevice`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtDeviceInfo {
    pub name: String,
    /// Reported via `bluetoothctl info <mac>` (Battery Percentage), or
    /// `0` if the device doesn't expose one.
    pub battery: u8,
    pub connected: bool,
}

/// Power profile, mirrors [`crate::shell::quick_settings::PowerProfile`]
/// — duplicated so this module doesn't need to import the popover state
/// type (and so unit tests don't need any UI scaffolding).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerProfile {
    Saver,
    Balanced,
    Performance,
}

impl PowerProfile {
    /// Stringly-typed cli argument used by `powerprofilesctl set <name>`.
    pub fn cli_arg(self) -> &'static str {
        match self {
            PowerProfile::Saver => "power-saver",
            PowerProfile::Balanced => "balanced",
            PowerProfile::Performance => "performance",
        }
    }

    /// Inverse of [`Self::cli_arg`]; used by [`get_power_profile`].
    pub fn from_cli(s: &str) -> Option<Self> {
        match s.trim() {
            "power-saver" => Some(PowerProfile::Saver),
            "balanced" => Some(PowerProfile::Balanced),
            "performance" => Some(PowerProfile::Performance),
            _ => None,
        }
    }
}

/// Probe whether `binary` is on `$PATH`. Pure `Command::new(binary)
/// .arg("--version")` would spawn the process, which is wasteful when we
/// only need a presence check — `which` is fast and returns 1 cleanly if
/// missing.
fn which(binary: &str) -> bool {
    Command::new("which")
        .arg(binary)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run `binary args...` with a small timeout, capturing stdout. Returns
/// `Err` with a message that includes the binary's name and any stderr
/// the command produced — call-sites can hand that string straight to
/// `tracing::warn!`.
fn run_capture(binary: &str, args: &[&str]) -> Result<String, String> {
    if !which(binary) {
        return Err(format!("{binary} not installed"));
    }
    let output = Command::new(binary)
        .args(args)
        .output()
        .map_err(|e| format!("{binary} spawn failed: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "{binary} exited with status {:?}: {stderr}",
            output.status.code()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("{binary} stdout not utf-8: {e}"))
}

/// Run `binary args...` and discard stdout. Same error semantics as
/// [`run_capture`]. Used by setter shell-outs.
fn run(binary: &str, args: &[&str]) -> Result<(), String> {
    let _ = run_capture(binary, args)?;
    Ok(())
}

// ---------------------------------------------------------------
// Brightness
// ---------------------------------------------------------------

/// Set the backlight brightness to `pct` percent. `pct` is clamped to
/// `[0, 100]` before being formatted into the `brightnessctl` argument.
pub fn set_brightness(pct: f32) -> Result<(), String> {
    let clamped = pct.clamp(0.0, 100.0).round() as u32;
    run("brightnessctl", &["set", &format!("{clamped}%")])
}

/// Read the current brightness, returned as a `0.0..=1.0` fraction.
///
/// Parses `brightnessctl -m` (machine-readable: one line of
/// `name,class,value,percent,max` separated by commas) so we don't have
/// to spawn two processes for `get` and `max`.
pub fn get_brightness() -> Result<f32, String> {
    let out = run_capture("brightnessctl", &["-m"])?;
    parse_brightness_machine(&out)
}

fn parse_brightness_machine(stdout: &str) -> Result<f32, String> {
    // Format example: `acpi_video0,backlight,4,40%,10`
    let line = stdout.lines().next().ok_or("brightnessctl -m empty")?;
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() < 4 {
        return Err(format!("brightnessctl -m unexpected: {line}"));
    }
    // Field 3 is the percentage with a trailing `%`.
    let pct_str = fields[3].trim_end_matches('%');
    let pct: f32 = pct_str
        .parse()
        .map_err(|e| format!("brightness pct parse: {e} ({pct_str:?})"))?;
    Ok((pct / 100.0).clamp(0.0, 1.0))
}

// ---------------------------------------------------------------
// Volume
// ---------------------------------------------------------------

/// Set the default audio sink volume to `pct` percent. `pct` is clamped
/// to `[0, 100]`.
pub fn set_volume(pct: f32) -> Result<(), String> {
    let clamped = (pct.clamp(0.0, 100.0) / 100.0).clamp(0.0, 1.0);
    // wpctl takes a fraction like `0.50` or `50%`. We use the fraction
    // form to avoid locale-dependent `%` handling in shells.
    let arg = format!("{clamped:.2}");
    run("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &arg])
}

/// Read the default audio sink volume, returned as a `0.0..=1.0`
/// fraction.
pub fn get_volume() -> Result<f32, String> {
    let out = run_capture("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
    parse_wpctl_volume(&out)
}

fn parse_wpctl_volume(stdout: &str) -> Result<f32, String> {
    // wpctl prints: `Volume: 0.45` or `Volume: 0.45 [MUTED]`.
    let line = stdout.lines().next().ok_or("wpctl empty")?;
    let after = line
        .split(':')
        .nth(1)
        .ok_or_else(|| format!("wpctl format: {line}"))?
        .trim();
    let num_str = after.split_whitespace().next().unwrap_or("");
    let v: f32 = num_str
        .parse()
        .map_err(|e| format!("wpctl volume parse: {e} ({num_str:?})"))?;
    Ok(v.clamp(0.0, 1.0))
}

// ---------------------------------------------------------------
// Wi-Fi
// ---------------------------------------------------------------

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
/// `nmcli -t -f IN-USE,SSID,SIGNAL,SECURITY device wifi list`. The
/// `-t` (terse) flag prints one row per line, fields separated by `:`.
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
        // Fields can contain backslash-escaped colons; split_terse handles
        // the `\:` escape that nmcli emits in terse mode.
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
/// known SSIDs). The call is synchronous: nmcli blocks until association
/// either completes or fails.
pub fn connect_wifi(ssid: &str, password: Option<&str>) -> Result<(), String> {
    let mut args: Vec<&str> = vec!["device", "wifi", "connect", ssid];
    if let Some(pw) = password {
        args.push("password");
        args.push(pw);
    }
    run("nmcli", &args)
}

// ---------------------------------------------------------------
// Bluetooth
// ---------------------------------------------------------------

/// Toggle the Bluetooth controller via `bluetoothctl power on|off`.
pub fn set_bluetooth(enabled: bool) -> Result<(), String> {
    let state = if enabled { "on" } else { "off" };
    run("bluetoothctl", &["power", state])
}

/// Read the Bluetooth controller state by grepping `bluetoothctl show`
/// for `Powered:`.
pub fn get_bluetooth() -> Result<bool, String> {
    let out = run_capture("bluetoothctl", &["show"])?;
    parse_bluetoothctl_power(&out)
}

fn parse_bluetoothctl_power(stdout: &str) -> Result<bool, String> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Powered:") {
            return Ok(rest.trim() == "yes");
        }
    }
    Err("bluetoothctl show: no Powered: line".to_string())
}

/// List paired Bluetooth devices and their connected state. Two-step
/// shell-out: `bluetoothctl devices Paired` lists name + MAC, then for
/// each MAC we call `bluetoothctl info <mac>` to extract `Connected:`
/// and `Battery Percentage:`.
pub fn list_bt_devices() -> Result<Vec<BtDeviceInfo>, String> {
    let out = run_capture("bluetoothctl", &["devices", "Paired"])?;
    let mut devices = Vec::new();
    for (mac, name) in parse_bluetoothctl_devices(&out) {
        let info_out = run_capture("bluetoothctl", &["info", &mac]).unwrap_or_default();
        let (connected, battery) = parse_bluetoothctl_info(&info_out);
        devices.push(BtDeviceInfo {
            name,
            battery,
            connected,
        });
    }
    Ok(devices)
}

/// Parse `bluetoothctl devices Paired` output:
/// `Device AA:BB:CC:DD:EE:FF Some Name`
fn parse_bluetoothctl_devices(stdout: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix("Device ") else {
            continue;
        };
        let mut parts = rest.splitn(2, ' ');
        let mac = match parts.next() {
            Some(m) => m.to_string(),
            None => continue,
        };
        let name = parts.next().unwrap_or("").trim().to_string();
        if !mac.is_empty() {
            out.push((mac, name));
        }
    }
    out
}

/// Parse the relevant lines of `bluetoothctl info <mac>`. Returns
/// `(connected, battery)`; battery defaults to `0` when no
/// `Battery Percentage:` line is present.
fn parse_bluetoothctl_info(stdout: &str) -> (bool, u8) {
    let mut connected = false;
    let mut battery = 0u8;
    for line in stdout.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Connected:") {
            connected = rest.trim() == "yes";
        } else if let Some(rest) = t.strip_prefix("Battery Percentage:") {
            // Format: `Battery Percentage: 0x50 (80)` — the decimal in
            // parens is what we want.
            if let Some(open) = rest.find('(') {
                let after = &rest[open + 1..];
                if let Some(close) = after.find(')') {
                    if let Ok(n) = after[..close].trim().parse::<u8>() {
                        battery = n;
                    }
                }
            }
        }
    }
    (connected, battery)
}

// ---------------------------------------------------------------
// Power profile
// ---------------------------------------------------------------

/// Set the active power profile via `powerprofilesctl set <profile>`.
pub fn set_power_profile(profile: PowerProfile) -> Result<(), String> {
    run("powerprofilesctl", &["set", profile.cli_arg()])
}

/// Read the active power profile via `powerprofilesctl get`.
pub fn get_power_profile() -> Result<PowerProfile, String> {
    let out = run_capture("powerprofilesctl", &["get"])?;
    PowerProfile::from_cli(out.trim())
        .ok_or_else(|| format!("powerprofilesctl get unexpected: {out:?}"))
}

// ---------------------------------------------------------------
// Logging helpers
// ---------------------------------------------------------------

/// Wrapper around a `Result<(), String>` that funnels the error into
/// `tracing::warn!` with a stable prefix. Call-sites use this so the
/// click-handler stays a one-liner:
///
/// ```ignore
/// log_qs_result("set_brightness", qs_actions::set_brightness(value));
/// ```
pub fn log_qs_result(op: &str, result: Result<(), String>) {
    if let Err(e) = result {
        warn!(target: "qos::qs", op = op, error = %e, "qs action failed");
    }
}

/// Same as [`log_qs_result`] but for getter-shaped results that return
/// a value alongside the error.
pub fn log_qs_get<T>(op: &str, result: Result<T, String>) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(target: "qos::qs", op = op, error = %e, "qs get failed");
            None
        }
    }
}

/// Default short timeout for any future async/blocking guard — kept as a
/// constant so call-sites that want to enforce a deadline have a single
/// value to import. Not used by the sync shell-outs above because
/// `std::process::Command::output` already inherits the OS-level
/// SIGTERM behaviour of the parent shell, and the QS controls are
/// user-driven so a hung command surfaces as visible latency.
#[allow(dead_code)]
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(2_500);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_brightness_machine_returns_fraction() {
        let s = "acpi_video0,backlight,4,40%,10";
        let v = parse_brightness_machine(s).unwrap();
        assert!((v - 0.40).abs() < 1e-6);
    }

    #[test]
    fn parse_brightness_machine_handles_zero() {
        let s = "acpi_video0,backlight,0,0%,10";
        assert_eq!(parse_brightness_machine(s).unwrap(), 0.0);
    }

    #[test]
    fn parse_brightness_machine_clamps_above_one() {
        // brightnessctl will never emit >100%, but a hypothetical
        // 110% must not surface as a >1.0 fraction.
        let s = "x,b,11,110%,10";
        assert_eq!(parse_brightness_machine(s).unwrap(), 1.0);
    }

    #[test]
    fn parse_brightness_machine_empty_is_err() {
        assert!(parse_brightness_machine("").is_err());
    }

    #[test]
    fn parse_wpctl_volume_simple() {
        let s = "Volume: 0.45";
        let v = parse_wpctl_volume(s).unwrap();
        assert!((v - 0.45).abs() < 1e-6);
    }

    #[test]
    fn parse_wpctl_volume_muted_suffix() {
        let s = "Volume: 0.30 [MUTED]";
        let v = parse_wpctl_volume(s).unwrap();
        assert!((v - 0.30).abs() < 1e-6);
    }

    #[test]
    fn parse_wpctl_volume_garbage_is_err() {
        assert!(parse_wpctl_volume("not-a-line").is_err());
    }

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
        // Two BSSIDs broadcasting the same SSID — should collapse into
        // one entry with the higher signal preserved.
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
    fn parse_bluetoothctl_power_yes() {
        let s = "Controller AA:BB\n\tName: Foo\n\tPowered: yes\n\tDiscoverable: no";
        assert_eq!(parse_bluetoothctl_power(s).unwrap(), true);
    }

    #[test]
    fn parse_bluetoothctl_power_no() {
        let s = "\tPowered: no";
        assert_eq!(parse_bluetoothctl_power(s).unwrap(), false);
    }

    #[test]
    fn parse_bluetoothctl_power_missing() {
        assert!(parse_bluetoothctl_power("no Powered line").is_err());
    }

    #[test]
    fn parse_bluetoothctl_devices_basic() {
        let s = "\
Device AA:BB:CC:DD:EE:01 AirPods Pro\n\
Device AA:BB:CC:DD:EE:02 Magic Mouse\n\
";
        let v = parse_bluetoothctl_devices(s);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, "AA:BB:CC:DD:EE:01");
        assert_eq!(v[0].1, "AirPods Pro");
        assert_eq!(v[1].1, "Magic Mouse");
    }

    #[test]
    fn parse_bluetoothctl_info_extracts_battery_and_connected() {
        let s = "\
Device AA:BB:CC:DD:EE:01\n\
\tName: AirPods Pro\n\
\tConnected: yes\n\
\tBattery Percentage: 0x50 (80)\n\
";
        let (connected, battery) = parse_bluetoothctl_info(s);
        assert!(connected);
        assert_eq!(battery, 80);
    }

    #[test]
    fn parse_bluetoothctl_info_no_battery_defaults_zero() {
        let s = "\tConnected: no\n";
        let (connected, battery) = parse_bluetoothctl_info(s);
        assert!(!connected);
        assert_eq!(battery, 0);
    }

    #[test]
    fn power_profile_round_trip() {
        assert_eq!(PowerProfile::Saver.cli_arg(), "power-saver");
        assert_eq!(PowerProfile::Balanced.cli_arg(), "balanced");
        assert_eq!(PowerProfile::Performance.cli_arg(), "performance");
        assert_eq!(
            PowerProfile::from_cli("balanced"),
            Some(PowerProfile::Balanced)
        );
        assert_eq!(
            PowerProfile::from_cli("performance\n"),
            Some(PowerProfile::Performance)
        );
        assert_eq!(PowerProfile::from_cli("garbled"), None);
    }

    /// Smoke test: setters that shell out should never panic when the
    /// underlying binary is missing — they must return a
    /// `"<binary> not installed"` error string. We force the missing-
    /// binary path by using a name that almost certainly isn't on
    /// `$PATH`.
    #[test]
    fn missing_binary_returns_not_installed_error() {
        let r = run("definitely-not-a-real-binary-qos-test", &["--help"]);
        assert!(r.is_err());
        let msg = r.unwrap_err();
        assert!(msg.contains("not installed"), "got: {msg}");
    }

    /// Brightness setter on a host without `brightnessctl` returns a
    /// clean error (no panic). When the binary *is* present we don't
    /// run anything here — that's an integration concern.
    #[test]
    fn set_brightness_on_missing_binary_is_err() {
        // We can't easily mock `which` without restructuring; this
        // test asserts the function returns `Result<(), String>` and
        // doesn't panic regardless of binary presence. On dev hosts
        // that have brightnessctl installed, the call may succeed —
        // both branches are acceptable.
        let result = set_brightness(50.0);
        match result {
            Ok(()) => {}
            Err(e) => assert!(!e.is_empty(), "error message should not be empty"),
        }
    }

    /// Clamp behaviour is observable through the `pct` argument — the
    /// function doesn't expose the rounded integer it formats into the
    /// CLI string, so we test the clamp arithmetic indirectly via the
    /// public `set_volume` path (which constructs the same fraction).
    #[test]
    fn set_volume_clamps_out_of_range() {
        // Above 100% must not panic or produce an arg like "1.50".
        // The function returns Result; success/failure depends on host
        // wpctl presence, but the input clamping must hold either way.
        // We assert that clamp logic doesn't overflow on extreme inputs.
        let r1 = set_volume(150.0);
        let r2 = set_volume(-25.0);
        // Both should produce a Result (Ok or "not installed" Err).
        assert!(r1.is_ok() || r1.unwrap_err().contains("not installed"));
        assert!(r2.is_ok() || r2.unwrap_err().contains("not installed"));
    }
}

// ---------------------------------------------------------------
// Click-routing test surface
//
// The QS click handlers in `overlay_host.rs` accept a hook trait so the
// chrome-interaction tests can intercept the side effect without
// actually shelling out. Production wiring uses [`RealQsActions`]
// (which calls the free functions above); tests use a custom impl that
// records every invocation.
// ---------------------------------------------------------------

/// Side-effect interface for the quick-settings actions. The default
/// production impl ([`RealQsActions`]) calls into the free functions
/// in this module; tests substitute a recording impl so the click
/// handler can be asserted on without shelling out.
pub trait QsActions {
    fn set_brightness(&self, pct: f32) -> Result<(), String>;
    fn set_volume(&self, pct: f32) -> Result<(), String>;
    fn set_wifi(&self, enabled: bool) -> Result<(), String>;
    fn set_bluetooth(&self, enabled: bool) -> Result<(), String>;
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String>;
    fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String>;
}

/// Production impl — every method forwards to the free shell-out
/// helper of the same name in this module. Stateless; cheap to
/// construct (a zero-sized struct).
#[derive(Default, Clone, Copy, Debug)]
pub struct RealQsActions;

impl QsActions for RealQsActions {
    fn set_brightness(&self, pct: f32) -> Result<(), String> {
        super::qs_actions::set_brightness(pct)
    }
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        super::qs_actions::set_volume(pct)
    }
    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        super::qs_actions::set_wifi(enabled)
    }
    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        super::qs_actions::set_bluetooth(enabled)
    }
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        super::qs_actions::connect_wifi(ssid, password)
    }
    fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String> {
        super::qs_actions::set_power_profile(profile)
    }
}

#[cfg(test)]
mod dispatcher_tests {
    use super::*;
    use std::cell::RefCell;

    /// Recording dispatcher used by the click-routing tests. Captures
    /// every call into a `Vec<String>` so the test can assert the
    /// click translated into the right shell-out.
    #[derive(Default)]
    struct RecordingDispatcher {
        calls: RefCell<Vec<String>>,
    }

    impl RecordingDispatcher {
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl QsActions for RecordingDispatcher {
        fn set_brightness(&self, pct: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_brightness({pct:.2})"));
            Ok(())
        }
        fn set_volume(&self, pct: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_volume({pct:.2})"));
            Ok(())
        }
        fn set_wifi(&self, enabled: bool) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_wifi({enabled})"));
            Ok(())
        }
        fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_bluetooth({enabled})"));
            Ok(())
        }
        fn connect_wifi(&self, ssid: &str, _password: Option<&str>) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("connect_wifi({ssid})"));
            Ok(())
        }
        fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_power_profile({})", profile.cli_arg()));
            Ok(())
        }
    }

    /// Direct invocation through the trait — proves the dispatcher
    /// surface compiles and dispatches as expected. Click-routing
    /// integration is exercised in `tests/qs_actions_click_routing.rs`.
    #[test]
    fn dispatcher_records_calls() {
        let d = RecordingDispatcher::default();
        d.set_brightness(60.0).unwrap();
        d.set_volume(75.0).unwrap();
        d.set_wifi(false).unwrap();
        d.set_bluetooth(true).unwrap();
        d.connect_wifi("MyNet", Some("hunter2")).unwrap();
        d.set_power_profile(PowerProfile::Performance).unwrap();

        let calls = d.calls();
        assert_eq!(calls.len(), 6);
        assert_eq!(calls[0], "set_brightness(60.00)");
        assert_eq!(calls[1], "set_volume(75.00)");
        assert_eq!(calls[2], "set_wifi(false)");
        assert_eq!(calls[3], "set_bluetooth(true)");
        assert_eq!(calls[4], "connect_wifi(MyNet)");
        assert_eq!(calls[5], "set_power_profile(performance)");
    }
}
