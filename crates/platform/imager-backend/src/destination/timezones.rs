//! Runtime time-zone discovery for the installer's time-zone picker.
//!
//! Resolution order, most authoritative first:
//!   1. `timedatectl list-timezones` — the canonical systemd list.
//!   2. Walking `/usr/share/zoneinfo` — works on any glibc system even
//!      without a running systemd (e.g. inside a minimal live medium).
//!   3. A small curated fallback so the picker is never empty (tests,
//!      odd containers).
//!
//! The returned list is sorted, de-duplicated, and always contains
//! [`DEFAULT_TIMEZONE`] (`UTC`) so the seeded default is selectable.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

/// The time zone seeded by default (kept in sync with the installer's
/// `installer.timezone` CRDT seed).
pub const DEFAULT_TIMEZONE: &str = "UTC";

/// A minimal, always-valid set used when neither `timedatectl` nor
/// `/usr/share/zoneinfo` is available.
const CURATED_FALLBACK: &[&str] = &[
    "UTC",
    "Europe/Amsterdam",
    "Europe/London",
    "Europe/Berlin",
    "Europe/Paris",
    "Europe/Madrid",
    "Europe/Moscow",
    "America/New_York",
    "America/Chicago",
    "America/Denver",
    "America/Los_Angeles",
    "America/Sao_Paulo",
    "Asia/Tokyo",
    "Asia/Shanghai",
    "Asia/Kolkata",
    "Asia/Dubai",
    "Australia/Sydney",
    "Pacific/Auckland",
];

/// Discover the system's IANA time-zone list. Never returns empty.
pub fn list_timezones() -> Vec<String> {
    if let Some(tz) = from_timedatectl()
        && !tz.is_empty()
    {
        return finalize(tz);
    }
    if let Some(tz) = from_zoneinfo(Path::new("/usr/share/zoneinfo"))
        && !tz.is_empty()
    {
        return finalize(tz);
    }
    finalize(
        CURATED_FALLBACK
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
    )
}

/// Sort, de-dup, and guarantee `UTC` is present.
fn finalize(zones: Vec<String>) -> Vec<String> {
    let mut set: BTreeSet<String> = zones.into_iter().collect();
    set.insert(DEFAULT_TIMEZONE.to_string());
    set.into_iter().collect()
}

fn from_timedatectl() -> Option<Vec<String>> {
    let out = bounded_command::run(
        "timedatectl",
        &["list-timezones"],
        None,
        1024 * 1024,
        Duration::from_secs(5),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let zones: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(std::string::ToString::to_string)
        .collect();
    Some(zones)
}

/// Walk `root` (`/usr/share/zoneinfo`) collecting zone names. A relative
/// path is treated as a zone when it passes [`is_zone_path`].
fn from_zoneinfo(root: &Path) -> Option<Vec<String>> {
    if !root.is_dir() {
        return None;
    }
    let mut zones = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file()
                && let Ok(rel) = path.strip_prefix(root)
            {
                let name = rel.to_string_lossy().replace('\\', "/");
                if is_zone_path(&name) {
                    zones.push(name);
                }
            }
        }
    }
    Some(zones)
}

/// Heuristic for "is this zoneinfo-relative path an actual zone name?".
/// Zones look like `Area/Location` (`Europe/Amsterdam`) or a bare
/// top-level (`UTC`). The directory holds non-zone helpers too —
/// `posix/`, `right/` (duplicate trees), `zone.tab`, `iso3166.tab`,
/// `leapseconds`, `tzdata.zi` — which we exclude.
fn is_zone_path(name: &str) -> bool {
    if name.starts_with("posix/") || name.starts_with("right/") {
        return false;
    }
    // Helper data files carry an extension or are lowercase tables.
    if name.contains('.') {
        return false;
    }
    // Real zone components start with an uppercase letter
    // (`Etc/UTC`, `America/New_York`); the lowercase helpers don't.
    match name.split('/').next() {
        Some(first) => first.chars().next().is_some_and(|c| c.is_ascii_uppercase()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timezone_catalog_lists_only_real_zones_sorted_with_utc() {
        {
            let out = finalize(vec![
                "Europe/Berlin".into(),
                "Europe/Berlin".into(),
                "Asia/Tokyo".into(),
            ]);
            assert!(out.contains(&"UTC".to_string()));
            assert_eq!(
                out,
                vec![
                    "Asia/Tokyo".to_string(),
                    "Europe/Berlin".to_string(),
                    "UTC".to_string()
                ]
            );
        }
        {
            assert!(is_zone_path("UTC"));
            assert!(is_zone_path("Europe/Amsterdam"));
            assert!(is_zone_path("America/Argentina/Buenos_Aires"));
            assert!(is_zone_path("Etc/GMT"));
        }
        {
            assert!(!is_zone_path("posix/UTC"));
            assert!(!is_zone_path("right/UTC"));
            assert!(!is_zone_path("zone.tab"));
            assert!(!is_zone_path("iso3166.tab"));
            assert!(!is_zone_path("tzdata.zi"));
            assert!(!is_zone_path("leapseconds")); // lowercase first component
        }
        {
            let tz = list_timezones();
            assert!(!tz.is_empty());
            assert!(tz.contains(&"UTC".to_string()));
        }
    }
}
