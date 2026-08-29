//! Time / NTP wrappers via `timedatectl` and `date`, shared by the
//! Settings time-date panel. Same design as the other modules: sync,
//! presence-checked shell-outs with no extra deps — formatting is
//! delegated to `date(1)` rather than pulling in a datetime crate.

use crate::process::{run, run_capture};

/// Is automatic time synchronisation (NTP) enabled?
/// `timedatectl show -p NTP --value` prints `yes` / `no`.
pub fn get_ntp() -> Result<bool, String> {
    let out = run_capture("timedatectl", &["show", "-p", "NTP", "--value"])?;
    parse_timedatectl_ntp(&out)
}

/// Enable/disable automatic time synchronisation. Needs privilege, so a
/// denial surfaces as the command's error string.
pub fn set_ntp(enabled: bool) -> Result<(), String> {
    run(
        "timedatectl",
        &["set-ntp", if enabled { "true" } else { "false" }],
    )
}

/// Current wall-clock time, formatted 24-hour (`14:05`) or 12-hour
/// (`02:05 PM`) per `h24`.
pub fn now_clock(h24: bool) -> Result<String, String> {
    let fmt = if h24 { "+%H:%M" } else { "+%I:%M %p" };
    Ok(run_capture("date", &[fmt])?.trim().to_string())
}

/// Current date, e.g. `Monday, 4 July 2026`.
pub fn now_date() -> Result<String, String> {
    Ok(run_capture("date", &["+%A, %-d %B %Y"])?.trim().to_string())
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
    fn parses_ntp_yes_no() {
        assert_eq!(parse_timedatectl_ntp("yes\n"), Ok(true));
        assert_eq!(parse_timedatectl_ntp("no\n"), Ok(false));
        assert!(parse_timedatectl_ntp("maybe").is_err());
    }
}
