//! About pane — the one pane with real data.
//!
//! Reads `/etc/os-release` (PRETTY_NAME), `/proc/sys/kernel/osrelease`
//! (running kernel version) and `/proc/uptime` (seconds since boot, first
//! whitespace-separated field). Every read is best-effort: a missing or
//! unreadable file falls back to the string `"unknown"` so the pane still
//! renders cleanly under `--shots` on minimal CI containers.

use std::fs;
use std::path::Path;
use std::time::Duration;
use ui::widget::{Column, Row, Spacer, Text, Widget};

/// Snapshot of the data the About pane reads at construction time.
///
/// Captured into a plain struct (rather than re-reading on every frame) so
/// tests can construct one with synthetic data without touching the
/// filesystem. The pane builder takes a `&AboutInfo` so the same widget
/// tree is shared between the live binary and the smoke tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AboutInfo {
    pub pretty_name: String,
    pub kernel: String,
    pub uptime: String,
}

impl AboutInfo {
    /// Sample the real system: `/etc/os-release`,
    /// `/proc/sys/kernel/osrelease`, `/proc/uptime`. Failures degrade to
    /// the string `"unknown"` for the missing field so the rest of the
    /// pane still renders.
    pub fn from_system() -> Self {
        Self::from_paths(
            Path::new("/etc/os-release"),
            Path::new("/proc/sys/kernel/osrelease"),
            Path::new("/proc/uptime"),
        )
    }

    /// Same as `from_system` but with caller-supplied paths — used by the
    /// unit tests to feed fixtures without monkeypatching the filesystem.
    pub fn from_paths(os_release: &Path, kernel_path: &Path, uptime_path: &Path) -> Self {
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
        Self { pretty_name, kernel, uptime }
    }
}

/// Build the pane body.
pub fn build() -> Box<dyn Widget> {
    build_with(&AboutInfo::from_system())
}

/// Build the pane body from caller-supplied data — used by tests and the
/// `--shots` mode if it ever needs deterministic content. Keeping the
/// pure-data builder separate from `build()` mirrors the
/// `file_explorer::sidebar::build_tiers` pattern (data → widget).
pub fn build_with(info: &AboutInfo) -> Box<dyn Widget> {
    let mut col = Column::new();
    col = col.push(Box::new(Text::new("System").with_size(14.0)));
    col = col.push(Box::new(field_row("OS", &info.pretty_name)));
    col = col.push(Box::new(field_row("Kernel", &info.kernel)));
    col = col.push(Box::new(field_row("Uptime", &info.uptime)));
    Box::new(col)
}

/// One name-value row inside the About pane.
fn field_row(label: &str, value: &str) -> Row {
    Row::new()
        .push(Box::new(Text::new(label).with_size(12.0)))
        .push(Box::new(Spacer::new()))
        .push(Box::new(Text::new(value).with_size(12.0)))
}

/// Extract `PRETTY_NAME` from `/etc/os-release`. The file is a
/// shell-style key=value list; values may be optionally quoted with
/// single or double quotes.
pub(crate) fn parse_os_release_pretty_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            let unquoted = rest
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            if !unquoted.is_empty() {
                return Some(unquoted);
            }
        }
    }
    None
}

/// `/proc/uptime` is two whitespace-separated floats: seconds since boot
/// and seconds of idle CPU. We only care about the first.
pub(crate) fn parse_uptime_seconds(contents: &str) -> Option<f64> {
    contents
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
}

/// Pretty-print uptime as `<n>d <h>h <m>m` (or smaller units when the
/// system is freshly booted) — matches the convention `top(1)` uses.
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
        assert_eq!(parse_os_release_pretty_name(s).as_deref(), Some("Fedora Linux 42"));
    }

    #[test]
    fn parses_pretty_name_without_quotes() {
        let s = "PRETTY_NAME=Fedora42\n";
        assert_eq!(parse_os_release_pretty_name(s).as_deref(), Some("Fedora42"));
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
        // 1 day + 2h + 3m = 86_400 + 7_200 + 180 = 93_780 s
        assert_eq!(format_uptime(93_780.0), "1d 2h 3m");
    }

    #[test]
    fn uptime_format_drops_days_under_24h() {
        // 2h + 3m
        assert_eq!(format_uptime(7_380.0), "2h 3m");
    }

    #[test]
    fn uptime_format_drops_hours_under_1h() {
        assert_eq!(format_uptime(420.0), "7m");
    }

    #[test]
    fn from_paths_uses_unknown_for_missing_files() {
        // Use paths that almost certainly don't exist.
        let missing = Path::new("/nonexistent/quickdesktop/settings/test/path/that/should/not/exist");
        let info = AboutInfo::from_paths(missing, missing, missing);
        assert_eq!(info.pretty_name, "unknown");
        assert_eq!(info.kernel, "unknown");
        assert_eq!(info.uptime, "unknown");
    }
}
