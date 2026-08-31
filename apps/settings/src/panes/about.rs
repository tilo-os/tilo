//! About / qOS Identity pane — device identity and transfer.
//!
//! Reads real system facts where they're cheap: `/etc/os-release`
//! (`PRETTY_NAME`), `/proc/sys/kernel/osrelease` (kernel),
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

use crate::widgets::{caption, card, hero, rows, value_row};

/// Snapshot of the data the About pane reads at construction time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AboutInfo {
    pub pretty_name: String,
    pub kernel: String,
    pub uptime: String,
    pub hostname: String,
    pub arch: String,
    pub cpu: String,
    pub memory: String,
    pub identity_name: Option<String>,
    pub identity_public_key: Option<[u8; 32]>,
}

impl AboutInfo {
    pub fn from_system() -> Self {
        let mut info = Self::from_paths(
            Path::new("/etc/os-release"),
            Path::new("/proc/sys/kernel/osrelease"),
            Path::new("/proc/uptime"),
            Path::new("/proc/sys/kernel/hostname"),
            Path::new("/proc/cpuinfo"),
            Path::new("/proc/meminfo"),
        );
        if let Ok(identity) = runtime_identity::load() {
            info.identity_name =
                Some(identity.display_name().trim().to_string()).filter(|name| !name.is_empty());
            info.identity_public_key = Some(identity.group_public_key());
        }
        info
    }

    pub fn from_paths(
        os_release: &Path,
        kernel_path: &Path,
        uptime_path: &Path,
        hostname_path: &Path,
        cpuinfo_path: &Path,
        meminfo_path: &Path,
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
            .map_or_else(|| "unknown".to_string(), format_uptime);
        let hostname = fs::read_to_string(hostname_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "this-device".to_string());
        let cpu = fs::read_to_string(cpuinfo_path)
            .ok()
            .and_then(|s| parse_cpu_model(&s))
            .unwrap_or_else(|| "unknown".to_string());
        let memory = fs::read_to_string(meminfo_path)
            .ok()
            .and_then(|s| parse_mem_total_kb(&s))
            .map_or_else(|| "unknown".to_string(), format_memory);
        // Compile-time target arch equals the running machine's arch — an
        // honest fact that needs no syscall.
        let arch = std::env::consts::ARCH.to_string();
        Self {
            pretty_name,
            kernel,
            uptime,
            hostname,
            arch,
            cpu,
            memory,
            identity_name: None,
            identity_public_key: None,
        }
    }

    /// A short, stable identity fingerprint derived from the hostname.
    /// In a real deployment this is the device's P2P public-key handle;
    /// derived deterministically here so the panel always shows one.
    pub fn fingerprint(&self) -> String {
        // The workspace identity is the real thing to fingerprint; the
        // hostname hash is only a stand-in until one has been provisioned.
        let fingerprint = self.identity_public_key.map_or_else(
            || {
                let mut hash: u64 = 1_469_598_103_934_665_603;
                for b in self.hostname.bytes() {
                    hash ^= u64::from(b);
                    hash = hash.wrapping_mul(1_099_511_628_211);
                }
                hash
            },
            |[a, b, c, d, e, f, g, h, ..]| u64::from_be_bytes([a, b, c, d, e, f, g, h]),
        );
        let hex = format!("{fingerprint:016x}");
        format!(
            "{}·{}·{}·{}",
            hex.get(0..4).unwrap_or_default(),
            hex.get(4..8).unwrap_or_default(),
            hex.get(8..12).unwrap_or_default(),
            hex.get(12..16).unwrap_or_default()
        )
    }
}

pub fn build() -> Box<dyn Widget> {
    build_with(&AboutInfo::from_system())
}

pub fn build_with(info: &AboutInfo) -> Box<dyn Widget> {
    // A device-identity hero: the machine's name in the accent, the OS beneath.
    let hero_block = hero("\u{f0322}", &info.hostname, &info.pretty_name); // laptop glyph

    let identity = card(
        "Workspace identity",
        rows(vec![
            value_row(
                "Person",
                info.identity_name.as_deref().unwrap_or("Not connected"),
            ),
            value_row("Identity", &info.fingerprint()),
            caption(
                "This is the identity selected for the current workspace. \
                 Apps receive its public profile; private signing material stays outside the sandbox.",
            ),
        ]),
    );

    let system = card(
        "System",
        rows(vec![
            value_row("OS", &info.pretty_name),
            value_row("Kernel", &info.kernel),
            value_row("Architecture", &info.arch),
            value_row("Uptime", &info.uptime),
        ]),
    );

    let hardware = card(
        "Hardware",
        rows(vec![
            value_row("Processor", &info.cpu),
            value_row("Memory", &info.memory),
        ]),
    );

    crate::widgets::cards(vec![hero_block, identity, system, hardware])
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

/// Pull the CPU model from `/proc/cpuinfo` — the first `model name` line.
pub(crate) fn parse_cpu_model(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some((key, val)) = line.split_once(':')
            && key.trim() == "model name"
        {
            let v = val.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Pull total memory (in kB) from `/proc/meminfo`'s `MemTotal` line.
pub(crate) fn parse_mem_total_kb(contents: &str) -> Option<u64> {
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok());
        }
    }
    None
}

/// Format a memory size in kB as a friendly `"n.n GB"` (or `"n MB"` when
/// under a gigabyte).
pub(crate) fn format_memory(kb: u64) -> String {
    let mb = kb as f64 / 1024.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
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
        let info = AboutInfo::from_paths(missing, missing, missing, missing, missing, missing);
        assert_eq!(info.pretty_name, "unknown");
        assert_eq!(info.kernel, "unknown");
        assert_eq!(info.uptime, "unknown");
        assert_eq!(info.hostname, "this-device");
        assert_eq!(info.cpu, "unknown");
        assert_eq!(info.memory, "unknown");
        // Architecture is a compile-time fact, always present.
        assert!(!info.arch.is_empty());
    }

    #[test]
    fn parses_cpu_model_first_match() {
        let s = "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 7 7840U\nprocessor\t: 1\nmodel name\t: AMD Ryzen 7 7840U\n";
        assert_eq!(parse_cpu_model(s).as_deref(), Some("AMD Ryzen 7 7840U"));
        assert!(parse_cpu_model("no model here\n").is_none());
    }

    #[test]
    fn parses_and_formats_memory() {
        // 16 GiB in kB.
        assert_eq!(
            parse_mem_total_kb("MemTotal:       16330124 kB\n"),
            Some(16_330_124)
        );
        assert_eq!(format_memory(16_330_124), "15.6 GB");
        assert_eq!(format_memory(512 * 1024), "512 MB");
        assert!(parse_mem_total_kb("SwapTotal: 0 kB\n").is_none());
    }

    #[test]
    fn fingerprint_is_stable_and_formatted() {
        let info = AboutInfo {
            pretty_name: "x".into(),
            kernel: "x".into(),
            uptime: "x".into(),
            hostname: "ada-laptop".into(),
            arch: "x86_64".into(),
            cpu: "x".into(),
            memory: "x".into(),
            identity_name: None,
            identity_public_key: None,
        };
        let fp = info.fingerprint();
        // 4 groups of 4 hex chars separated by middots.
        assert_eq!(fp.matches('\u{00B7}').count(), 3);
        assert_eq!(info.fingerprint(), fp, "deterministic");
    }

    #[test]
    fn workspace_identity_fingerprint_comes_from_public_key() {
        let mut info = AboutInfo::from_paths(
            Path::new("/missing"),
            Path::new("/missing"),
            Path::new("/missing"),
            Path::new("/missing"),
            Path::new("/missing"),
            Path::new("/missing"),
        );
        info.identity_public_key = Some([0xab; 32]);
        assert_eq!(info.fingerprint(), "abab·abab·abab·abab");
    }
}
