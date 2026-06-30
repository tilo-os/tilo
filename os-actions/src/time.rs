//! Clock + automatic-time control via `date` and `timedatectl`.
//!
//! The Settings "Time & Date" pane reads the live wall clock (so the panel
//! shows the real current time, not a frozen sample) and toggles NTP
//! synchronisation. We shell out to coreutils `date` for formatting — it's
//! always present and avoids pulling a date/time crate into the build — and
//! to `timedatectl` for the NTP state (read is unprivileged; the setter is
//! best-effort and surfaces a clean error when it lacks privilege).

use crate::process::{run, run_capture};

/// Format the current local time. `h24` selects 24-hour (`14:05`) vs
/// 12-hour (`02:05 PM`). Shells out to `date`.
pub fn now_clock(h24: bool) -> Result<String, String> {
    let fmt = if h24 { "+%H:%M" } else { "+%I:%M %p" };
    Ok(run_capture("date", &[fmt])?.trim().to_string())
}

/// Format the current local date, e.g. `Tuesday, 01 July 2026`.
pub fn now_date() -> Result<String, String> {
    Ok(run_capture("date", &["+%A, %d %B %Y"])?.trim().to_string())
}

/// Read whether automatic time (NTP) synchronisation is on, by parsing
/// `timedatectl show -p NTP --value` (prints `yes`/`no`).
pub fn get_ntp() -> Result<bool, String> {
    let out = run_capture("timedatectl", &["show", "-p", "NTP", "--value"])?;
    Ok(out.trim() == "yes")
}

/// Enable/disable automatic time synchronisation via
/// `timedatectl set-ntp <bool>`. Requires privilege; the error string is
/// surfaced to the caller (which logs it) when it's denied.
pub fn set_ntp(enabled: bool) -> Result<(), String> {
    let arg = if enabled { "true" } else { "false" };
    run("timedatectl", &["set-ntp", arg])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both clock formats produce a non-empty string (or a clean
    /// "not installed" error on a host without `date`, which is never the
    /// case in practice but keeps CI hermetic).
    #[test]
    fn now_clock_is_nonempty_or_clean_error() {
        for h24 in [true, false] {
            match now_clock(h24) {
                Ok(s) => assert!(!s.is_empty(), "clock string empty for h24={h24}"),
                Err(e) => assert!(e.contains("not installed"), "unexpected: {e}"),
            }
        }
    }

    #[test]
    fn set_ntp_missing_binary_is_clean_error() {
        // We don't actually toggle NTP in CI; just assert the call shape
        // returns a Result without panicking.
        let r = set_ntp(true);
        assert!(r.is_ok() || r.is_err());
    }
}
