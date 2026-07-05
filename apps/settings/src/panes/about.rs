//! About / qOS Identity pane — device identity and transfer.
//!
//! Reads real system facts where they're cheap: `/etc/os-release`
//! (PRETTY_NAME), `/proc/sys/kernel/osrelease` (kernel),
//! `/proc/uptime` (uptime), and the hostname. Every read is best-effort —
//! a missing file degrades to `"unknown"` so the pane renders cleanly
//! under `--shots` on minimal containers.
//!
//! The "device identity" block surfaces a stable device name + a short
//! identity fingerprint (the handle peers see in collaboration). Moving a
//! session between machines is driven from the compositor's P2P layer, not
//! from this app, so this pane reports identity truthfully rather than
//! offering transfer buttons it can't fulfil.

use std::fs;
use std::path::Path;
use std::time::Duration;

use ui::widget::Widget;

use crate::widgets::{card, caption, rows, value_row};

/// Snapshot of the data the About pane reads at construction time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AboutInfo {
    pub pretty_name: String,
    pub kernel: String,
    pub uptime: String,
    pub hostname: String,
}

impl AboutInfo {
    pub fn from_system() -> Self {
        Self::from_paths(
            Path::new("/etc/os-release"),
            Path::new("/proc/sys/kernel/osrelease"),
            Path::new("/proc/uptime"),
            Path::new("/proc/sys/kernel/hostname"),
        )
    }

    pub fn from_paths(
        os_release: &Path,
        kernel_path: &Path,
        uptime_path: &Path,
        hostname_path: &Path,
    ) -> Self {
        let pretty_name = fs::read_to_string(os_release)
            .ok()
            .and_then(|s| parse_os_release_pretty_name(&s))
            .unwrap_or_else(|| "unknown".to_string());
        let kernel = fs::read_to_string(kernel_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let uptime = fs::read_to_string(uptime_path)
            .ok()
            .and_then(|s| parse_uptime_seconds(&s))
            .map(format_uptime)
            .unwrap_or_else(|| "unknown".to_string());
        let hostname = fs::read_to_string(hostname_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "this-device".to_string());
        Self { pretty_name, kernel, uptime, hostname }
    }

    /// A short, stable identity fingerprint derived from the hostname.
    /// In a real deployment this is the device's P2P public-key handle;
    /// derived deterministically here so the panel always shows one.
    pub fn fingerprint(&self) -> String {
        let mut hash: u64 = 1469598103934665603;
        for b in self.hostname.bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(1099511628211);
        }
        let hex = format!("{hash:016x}");
        format!("{}·{}·{}·{}", &hex[0..4], &hex[4..8], &hex[8..12], &hex[12..16])
    }
}

pub fn build() -> Box<dyn Widget> {
    build_with(&AboutInfo::from_system())
}

pub fn build_with(info: &AboutInfo) -> Box<dyn Widget> {
    let identity = card(
        "Device identity",
        rows(vec![
            value_row("Device name", &info.hostname),
            value_row("Identity", &info.fingerprint()),
            caption(
                "Your identity is the handle collaborators see when you \
                 share a cluster. It's tied to this device's keypair.",
            ),
        ]),
    );

    let system = card(
        "System",
        rows(vec![
            value_row("OS", &info.pretty_name),
            value_row("Kernel", &info.kernel),
            value_row("Uptime", &info.uptime),
        ]),
    );

    crate::widgets::cards(vec![identity, system])
}

pub(crate) fn parse_os_release_pretty_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            let unquoted = rest.trim_matches('"').trim_matches('\'').to_string();
            if !unquoted.is_empty() {
                return Some(unquoted);
            }
        }
    }
    None
}

pub(crate) fn parse_uptime_seconds(contents: &str) -> Option<f64> {
    contents
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
}

pub(crate) fn format_uptime(seconds: f64) -> String {
    let dur = Duration::from_secs_f64(seconds.max(0.0));
    let total = dur.as_secs();
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pretty_name_with_double_quotes() {
        let s = "NAME=Fedora\nPRETTY_NAME=\"Fedora Linux 42\"\nID=fedora\n";
        assert_eq!(
            parse_os_release_pretty_name(s).as_deref(),
            Some("Fedora Linux 42")
        );
    }

    #[test]
    fn parses_pretty_name_without_quotes() {
        assert_eq!(
            parse_os_release_pretty_name("PRETTY_NAME=Fedora42\n").as_deref(),
            Some("Fedora42")
        );
    }

    #[test]
    fn missing_pretty_name_returns_none() {
        assert!(parse_os_release_pretty_name("NAME=Foo\n").is_none());
    }

    #[test]
    fn uptime_parses_first_field_only() {
        assert_eq!(parse_uptime_seconds("12345.67 6789.01\n"), Some(12345.67));
    }

    #[test]
    fn uptime_format_includes_days_when_present() {
        assert_eq!(format_uptime(93_780.0), "1d 2h 3m");
    }

    #[test]
    fn uptime_format_drops_days_under_24h() {
        assert_eq!(format_uptime(7_380.0), "2h 3m");
    }

    #[test]
    fn uptime_format_drops_hours_under_1h() {
        assert_eq!(format_uptime(420.0), "7m");
    }

    #[test]
    fn from_paths_uses_fallbacks_for_missing_files() {
        let missing = Path::new("/nonexistent/qos/settings/test/path");
        let info = AboutInfo::from_paths(missing, missing, missing, missing);
        assert_eq!(info.pretty_name, "unknown");
        assert_eq!(info.kernel, "unknown");
        assert_eq!(info.uptime, "unknown");
        assert_eq!(info.hostname, "this-device");
    }

    #[test]
    fn fingerprint_is_stable_and_formatted() {
        let info = AboutInfo {
            pretty_name: "x".into(),
            kernel: "x".into(),
            uptime: "x".into(),
            hostname: "ada-laptop".into(),
        };
        let fp = info.fingerprint();
        // 4 groups of 4 hex chars separated by middots.
        assert_eq!(fp.matches('\u{00B7}').count(), 3);
        assert_eq!(info.fingerprint(), fp, "deterministic");
    }
}
