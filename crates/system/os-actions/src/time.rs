//! Time / NTP wrappers via `timedatectl` and `date`, shared by the
//! Settings time-date panel. Same design as the other modules: sync,
//! presence-checked shell-outs with no extra deps — formatting is
//! delegated to `date(1)` rather than pulling in a datetime crate.

use crate::process::{run, run_capture};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimeSyncStatus {
    pub timezone: Option<String>,
    pub ntp_enabled: Option<bool>,
    pub synchronized: Option<bool>,
    pub source: Option<String>,
}

/// Read timedated's authoritative state and, when systemd-timesyncd is the
/// provider, its selected server. Missing providers remain `None`; Settings
/// can distinguish unavailable from disabled.
///
/// # Errors
///
/// Returns an error message if `timedatectl` is not installed or timedated
/// cannot be asked; a missing time-sync provider is not an error.
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

/// The IANA zones timedated accepts. This uses the system zoneinfo database,
/// so the list follows tzdata updates instead of an embedded copy.
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

/// Set the system time zone to the IANA identifier `timezone`.
///
/// # Errors
///
/// Returns an error message if `timezone` fails [`valid_timezone`], or if `timedatectl` is not
/// installed, is not authorised, or the command otherwise fails.
pub fn set_timezone(timezone: &str) -> Result<(), String> {
    if !valid_timezone(timezone) {
        return Err("invalid IANA time-zone identifier".into());
    }
    // timedated validates membership in the installed tzdata set and polkit
    // authorises the active session.
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

/// Is automatic time synchronisation (NTP) enabled?
/// `timedatectl show -p NTP --value` prints `yes` / `no`.
pub fn get_ntp() -> Result<bool, String> {
    let out = run_capture("timedatectl", &["show", "-p", "NTP", "--value"])?;
    parse_timedatectl_ntp(&out)
}

/// Enable/disable automatic time synchronisation. Needs privilege, so a
/// denial surfaces as the command's error string.
///
/// # Errors
///
/// Returns an error message if `timedatectl` is not installed, is not authorised, or the
/// command otherwise fails.
pub fn set_ntp(enabled: bool) -> Result<(), String> {
    run(
        "timedatectl",
        &["set-ntp", if enabled { "true" } else { "false" }],
    )
}

/// Parse the `yes`/`no` value from `timedatectl show -p NTP --value`.
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
}
