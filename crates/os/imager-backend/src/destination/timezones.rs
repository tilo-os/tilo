//! Runtime time-zone discovery for the installer's time-zone picker.
//!
//! Resolution order: `timedatectl list-timezones`, then a walk of `/usr/share/zoneinfo`, then a small
//! curated fallback so the picker is never empty. The list is sorted, de-duplicated and always
//! contains [`DEFAULT_TIMEZONE`].

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

/// Kept in sync with the installer's `installer.timezone` seed.
pub const DEFAULT_TIMEZONE: &str = "UTC";

/// Used when neither `timedatectl` nor `/usr/share/zoneinfo` is available.
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

/// A relative path is treated as a zone when it passes [`is_zone_path`].
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

/// Zones look like `Area/Location` (`Europe/Amsterdam`) or a bare top-level (`UTC`); the helper data
/// files `posix/`, `right/`, `zone.tab`, `iso3166.tab`, `leapseconds` and `tzdata.zi` are excluded.
fn is_zone_path(name: &str) -> bool {
    if name.starts_with("posix/") || name.starts_with("right/") {
        return false;
    }
    if name.contains('.') {
        return false;
    }
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
