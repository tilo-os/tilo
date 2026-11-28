//! Time and NTP through `timedatectl`, shared by the Settings time panel; formatting is
//! `date(1)`'s rather than a datetime crate's.

use crate::process::{run, run_capture};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimeSyncStatus {
    pub timezone: Option<String>,
    pub ntp_enabled: Option<bool>,
    pub synchronized: Option<bool>,
    pub source: Option<String>,
}

/// timedated's state, and its selected server when systemd-timesyncd provides it. A missing provider
/// stays `None`, so Settings can tell unavailable from disabled.
pub fn get_time_sync_status() -> Result<TimeSyncStatus, String> {
    let mut status = parse_time_status(&run_capture(
        "timedatectl",
        &[
            "show",
            "-p",
            "Timezone",
            "-p",
            "NTP",
            "-p",
            "NTPSynchronized",
        ],
    )?);
    status.source = run_capture("timedatectl", &["timesync-status"])
        .ok()
        .and_then(|out| parse_time_source(&out));
    Ok(status)
}

/// The IANA zones timedated accepts, from the system zoneinfo database, so the list follows tzdata
/// rather than an embedded copy.
pub fn list_timezones() -> Result<Vec<String>, String> {
    let out = run_capture("timedatectl", &["list-timezones"])?;
    let mut zones: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|zone| !zone.is_empty() && !zone.contains(char::is_whitespace))
        .map(str::to_string)
        .collect();
    zones.sort();
    zones.dedup();
    Ok(zones)
}

/// The zone files that list where each zone's reference place stands: the denser table first.
pub const ZONE_TABLES: [&str; 2] = [
    "/usr/share/zoneinfo/zone1970.tab",
    "/usr/share/zoneinfo/zone.tab",
];

/// One IANA zone and the place its clock is kept for, in degrees north and east: where a map
/// puts it.
#[derive(Clone, Debug, PartialEq)]
pub struct ZonePosition {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

/// Every zone the system names, with where it stands, from the zoneinfo tables. Empty when neither
/// table is readable.
#[must_use]
pub fn list_zone_positions() -> Vec<ZonePosition> {
    for table in ZONE_TABLES {
        let Ok(text) = std::fs::read_to_string(table) else {
            continue;
        };
        let zones = positions_in(&text);
        if !zones.is_empty() {
            return zones;
        }
    }
    Vec::new()
}

/// The zones `table` lists, each once, in name order.
fn positions_in(table: &str) -> Vec<ZonePosition> {
    let mut seen = std::collections::BTreeSet::new();
    let mut zones = Vec::new();
    for line in table.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let _countries = fields.next();
        let (Some(coords), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        if !valid_timezone(name) || !seen.insert(name.to_owned()) {
            continue;
        }
        let Some((latitude, longitude)) = parse_coordinates(coords) else {
            continue;
        };
        zones.push(ZonePosition {
            name: name.to_owned(),
            latitude,
            longitude,
        });
    }
    zones.sort_by(|a, b| a.name.cmp(&b.name));
    zones
}

/// The degrees `text` writes as `+DDMM[SS]+DDDMM[SS]`.
fn parse_coordinates(text: &str) -> Option<(f64, f64)> {
    let rest = text.get(1..)?;
    let split = rest.find(['+', '-'])?.checked_add(1)?;
    let latitude = text.get(..split)?;
    let longitude = text.get(split..)?;
    Some((degrees(latitude, 2)?, degrees(longitude, 3)?))
}

/// `field` as `+` or `-` and `deg_digits` degrees, then minutes and seconds when they are given.
fn degrees(field: &str, deg_digits: usize) -> Option<f64> {
    let sign = match field.as_bytes().first()? {
        b'+' => 1.0,
        b'-' => -1.0,
        _ => return None,
    };
    let digits = field.get(1..)?;
    let fit = match deg_digits {
        2 => matches!(digits.len(), 4 | 6),
        _ => matches!(digits.len(), 5 | 7),
    };
    if !fit || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let whole = digits.get(..deg_digits)?;
    let rest = digits.get(deg_digits..)?;
    let whole: f64 = whole.parse().ok()?;
    let minutes: f64 = rest.get(0..2)?.parse().ok()?;
    let seconds: f64 = match rest.get(2..4) {
        Some(seconds) => seconds.parse().ok()?,
        None => 0.0,
    };
    Some(sign * (whole + minutes / 60.0 + seconds / 3600.0))
}

pub fn set_timezone(timezone: &str) -> Result<(), String> {
    if !valid_timezone(timezone) {
        return Err("invalid IANA time-zone identifier".into());
    }
    // timedated validates membership in the installed tzdata set and polkit authorises the active
    // session.
    run("timedatectl", &["set-timezone", timezone])
}

pub fn valid_timezone(timezone: &str) -> bool {
    !timezone.is_empty()
        && timezone.len() <= 128
        && !timezone.starts_with('/')
        && !timezone.contains("..")
        && timezone
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'+'))
}

fn parse_time_status(out: &str) -> TimeSyncStatus {
    let mut status = TimeSyncStatus::default();
    for line in out.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Timezone" if valid_timezone(value.trim()) => {
                status.timezone = Some(value.trim().to_string());
            }
            "NTP" => status.ntp_enabled = parse_yes_no(value),
            "NTPSynchronized" => status.synchronized = parse_yes_no(value),
            _ => {}
        }
    }
    status
}

fn parse_yes_no(value: &str) -> Option<bool> {
    match value.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

fn parse_time_source(out: &str) -> Option<String> {
    for line in out.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("Server:") {
            let source = value.trim();
            if !source.is_empty() {
                return Some(source.chars().take(160).collect());
            }
        }
    }
    None
}

pub fn get_ntp() -> Result<bool, String> {
    let out = run_capture("timedatectl", &["show", "-p", "NTP", "--value"])?;
    parse_timedatectl_ntp(&out)
}

/// Needs privilege, so a denial surfaces as the command's error string.
pub fn set_ntp(enabled: bool) -> Result<(), String> {
    run(
        "timedatectl",
        &["set-ntp", if enabled { "true" } else { "false" }],
    )
}

fn parse_timedatectl_ntp(stdout: &str) -> Result<bool, String> {
    match stdout.trim() {
        "yes" => Ok(true),
        "no" => Ok(false),
        other => Err(format!("timedatectl NTP unexpected: {other:?}")),
    }
}

#[cfg(test)]
mod time_tests {
    use super::*;

    #[test]
    fn timedated_output_parses_without_guessing_and_zone_ids_are_validated() {
        {
            assert_eq!(parse_timedatectl_ntp("yes\n"), Ok(true));
            assert_eq!(parse_timedatectl_ntp("no\n"), Ok(false));
            assert!(parse_timedatectl_ntp("maybe").is_err());
        }
        {
            let status = parse_time_status("Timezone=Europe/Berlin\nNTP=yes\nNTPSynchronized=no\n");
            assert_eq!(status.timezone.as_deref(), Some("Europe/Berlin"));
            assert_eq!(status.ntp_enabled, Some(true));
            assert_eq!(status.synchronized, Some(false));
            assert_eq!(parse_time_status("NTP=n/a").ntp_enabled, None);
        }
        {
            assert!(valid_timezone("America/Argentina/Buenos_Aires"));
            assert!(!valid_timezone("../../etc/passwd"));
            assert!(!valid_timezone("/etc/localtime"));
            assert!(!valid_timezone("Europe/Berlin;reboot"));
        }
        {
            assert_eq!(
                parse_time_source("       Server: 192.0.2.1 (time.example)\n"),
                Some("192.0.2.1 (time.example)".into())
            );
        }
    }

    /// The zone tables' degrees and minutes become the place a map marks, each zone once.
    #[test]
    fn zone_positions_read_degrees_minutes_and_seconds() {
        let zones = positions_in(
            "# tz database\n\
             NL\t+5222+00454\tEurope/Amsterdam\tNetherlands\n\
             JP\t+353916+1394441\tAsia/Tokyo\n\
             US\t+404251-0740023\tAmerica/New_York\tEastern\n\
             NL\t+5222+00454\tEurope/Amsterdam\tagain\n",
        );
        assert_eq!(
            zones
                .iter()
                .map(|zone| zone.name.as_str())
                .collect::<Vec<_>>(),
            ["America/New_York", "Asia/Tokyo", "Europe/Amsterdam"],
        );
        let amsterdam = zones.iter().find(|z| z.name == "Europe/Amsterdam").unwrap();
        assert!((amsterdam.latitude - 52.366_666).abs() < 0.000_1);
        assert!((amsterdam.longitude - 4.9).abs() < 0.000_1);
        let tokyo = zones.iter().find(|z| z.name == "Asia/Tokyo").unwrap();
        assert!((tokyo.latitude - 35.654_444).abs() < 0.000_1);
        assert!((tokyo.longitude - 139.744_722).abs() < 0.000_1);
        let new_york = zones.iter().find(|z| z.name == "America/New_York").unwrap();
        assert!(new_york.latitude > 40.0 && new_york.longitude < -74.0);
    }
}
