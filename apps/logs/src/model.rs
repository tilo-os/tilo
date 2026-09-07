//! Pure data model for the Logs viewer — syslog priorities, a single
//! journal entry, and the filter predicate. No rendering, no IO.
//!
//! A [`LogEntry`] is a flattened view of one `journalctl -o json` record:
//! the handful of fields the UI promotes to columns plus every remaining
//! field kept verbatim in [`LogEntry::extra`] for the detail pane.

use vello::peniko::Color;

use crate::theme_ext::ThemeColors;

/// The eight syslog severities, ordered most-severe (`Emerg`, level 0) to
/// least-severe (`Debug`, level 7). Level numbers match RFC 5424 and the
/// integer in the journal's `PRIORITY` field.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, serde::Serialize)]
pub enum Priority {
    Emerg,
    Alert,
    Crit,
    Err,
    Warning,
    Notice,
    Info,
    Debug,
}

impl Priority {
    /// Every priority, severe → verbose. Drives the severity threshold
    /// chips and the per-row / detail severity colouring.
    pub const ALL: [Priority; 8] = [
        Priority::Emerg,
        Priority::Alert,
        Priority::Crit,
        Priority::Err,
        Priority::Warning,
        Priority::Notice,
        Priority::Info,
        Priority::Debug,
    ];

    /// Map a journal `PRIORITY` integer to a [`Priority`]. Anything out of
    /// the 0..=7 range clamps to `Debug` (the most verbose bucket), which
    /// is the safe default for an unknown record.
    pub fn from_level(n: i64) -> Priority {
        match n {
            0 => Priority::Emerg,
            1 => Priority::Alert,
            2 => Priority::Crit,
            3 => Priority::Err,
            4 => Priority::Warning,
            5 => Priority::Notice,
            6 => Priority::Info,
            _ => Priority::Debug,
        }
    }

    /// The RFC 5424 level number (0 = emerg … 7 = debug).
    pub fn level(self) -> i64 {
        match self {
            Priority::Emerg => 0,
            Priority::Alert => 1,
            Priority::Crit => 2,
            Priority::Err => 3,
            Priority::Warning => 4,
            Priority::Notice => 5,
            Priority::Info => 6,
            Priority::Debug => 7,
        }
    }

    /// Short uppercase tag for the list column (`ERR`, `WARN`, …).
    pub fn tag(self) -> &'static str {
        match self {
            Priority::Emerg => "EMERG",
            Priority::Alert => "ALERT",
            Priority::Crit => "CRIT",
            Priority::Err => "ERR",
            Priority::Warning => "WARN",
            Priority::Notice => "NOTICE",
            Priority::Info => "INFO",
            Priority::Debug => "DEBUG",
        }
    }

    /// Human-readable name for the detail pane badge.
    pub fn long(self) -> &'static str {
        match self {
            Priority::Emerg => "Emergency",
            Priority::Alert => "Alert",
            Priority::Crit => "Critical",
            Priority::Err => "Error",
            Priority::Warning => "Warning",
            Priority::Notice => "Notice",
            Priority::Info => "Info",
            Priority::Debug => "Debug",
        }
    }

    /// Severity colour, resolved entirely from the OS theme's *semantic*
    /// palette so the viewer stays consistent across themes. Emergencies
    /// through errors share the danger hue (differentiated by their tag),
    /// warnings use the warning hue, and the verbose tail walks
    /// notice→info→debug across accent / info / dim neutrals.
    pub fn color(self, theme: &ThemeColors) -> Color {
        match self {
            Priority::Emerg | Priority::Alert | Priority::Crit | Priority::Err => theme.danger,
            Priority::Warning => theme.warning,
            Priority::Notice => theme.accent,
            Priority::Info => theme.info,
            Priority::Debug => theme.dim,
        }
    }
}

/// One flattened journal record.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LogEntry {
    /// `__REALTIME_TIMESTAMP` — microseconds since the Unix epoch (UTC).
    pub realtime_us: u64,
    /// `_SYSTEMD_UNIT`, falling back to the syslog identifier / comm.
    pub unit: String,
    /// `SYSLOG_IDENTIFIER` — the program name.
    pub identifier: String,
    pub priority: Priority,
    pub message: String,
    pub pid: Option<u32>,
    pub hostname: String,
    /// `_BOOT_ID` — groups entries by boot.
    pub boot_id: String,
    /// `_TRANSPORT` — journald, kernel, stdout, syslog, …
    pub transport: String,
    /// Every other field, kept for the detail pane in insertion order.
    pub extra: Vec<(String, String)>,
}

impl LogEntry {
    /// `Mon DD HH:MM:SS.mmm` in UTC — the list-column timestamp.
    pub fn timestamp(&self) -> String {
        let (y, mo, d, hh, mm, ss, milli) = civil(self.realtime_us);
        let _ = y;
        format!(
            "{} {:02} {:02}:{:02}:{:02}.{:03}",
            MONTHS.get((mo as usize).saturating_sub(1)).unwrap_or(&""),
            d,
            hh,
            mm,
            ss,
            milli
        )
    }

    /// `YYYY-MM-DD HH:MM:SS.mmm UTC` — the verbose detail-pane stamp.
    pub fn timestamp_full(&self) -> String {
        let (y, mo, d, hh, mm, ss, milli) = civil(self.realtime_us);
        format!("{y:04}-{mo:02}-{d:02} {hh:02}:{mm:02}:{ss:02}.{milli:03} UTC")
    }

    /// `HH:MM:SS` for compact contexts.
    pub fn time_hms(&self) -> String {
        let (_, _, _, hh, mm, ss, _) = civil(self.realtime_us);
        format!("{hh:02}:{mm:02}:{ss:02}")
    }

    /// The unit label shown in the list — strips a trailing `.service`
    /// for density, leaves other suffixes (`.socket`, `.timer`) intact.
    pub fn unit_short(&self) -> &str {
        self.unit.strip_suffix(".service").unwrap_or(&self.unit)
    }
}

/// The live filter applied to the entry list. Derived from CRDT view
/// state on every sync; pure predicate logic lives in [`Filter::passes`].
#[derive(Clone, Debug)]
pub struct Filter {
    /// Show entries with `priority.level() <= min_priority`. 7 = show all,
    /// 3 = errors-and-worse, etc.
    pub min_priority: i64,
    /// Exact unit match, or empty for "all units".
    pub unit: String,
    /// Exact `_BOOT_ID` match, or empty for "all boots".
    pub boot: String,
    /// Case-insensitive substring over message + unit + identifier.
    pub search: String,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            min_priority: 7,
            unit: String::new(),
            boot: String::new(),
            search: String::new(),
        }
    }
}

impl Filter {
    /// True when `entry` survives every active facet of this filter.
    pub fn passes(&self, entry: &LogEntry) -> bool {
        if entry.priority.level() > self.min_priority {
            return false;
        }
        if !self.unit.is_empty() && entry.unit != self.unit {
            return false;
        }
        if !self.boot.is_empty() && entry.boot_id != self.boot {
            return false;
        }
        if !self.search.is_empty() {
            let q = self.search.to_lowercase();
            let hit = entry.message.to_lowercase().contains(&q)
                || entry.unit.to_lowercase().contains(&q)
                || entry.identifier.to_lowercase().contains(&q);
            if !hit {
                return false;
            }
        }
        true
    }

    /// True when no facet is constraining the result set.
    pub fn is_clear(&self) -> bool {
        self.min_priority >= 7
            && self.unit.is_empty()
            && self.boot.is_empty()
            && self.search.is_empty()
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Convert epoch-microseconds to a civil `(year, month, day, hour, min,
/// sec, milli)` tuple in UTC. Uses Howard Hinnant's `civil_from_days`
/// algorithm so we need no date crate and stay deterministic offline.
fn civil(realtime_us: u64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let secs = (realtime_us / 1_000_000) as i64;
    let milli = ((realtime_us / 1_000) % 1_000) as u32;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hh = (rem / 3_600) as u32;
    let mm = ((rem % 3_600) / 60) as u32;
    let ss = (rem % 60) as u32;

    // civil_from_days: days since 1970-01-01 → (y, m, d).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d, hh, mm, ss, milli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_round_trips_through_level() {
        for p in Priority::ALL {
            assert_eq!(Priority::from_level(p.level()), p);
        }
    }

    #[test]
    fn out_of_range_priority_clamps_to_debug() {
        assert_eq!(Priority::from_level(42), Priority::Debug);
        assert_eq!(Priority::from_level(-1), Priority::Debug);
    }

    #[test]
    fn civil_decodes_a_known_instant() {
        // 2021-01-01 00:00:00.000 UTC = 1609459200 s.
        let us = 1_609_459_200_000_000;
        let (y, mo, d, hh, mm, ss, ms) = civil(us);
        assert_eq!((y, mo, d, hh, mm, ss, ms), (2021, 1, 1, 0, 0, 0, 0));
    }

    #[test]
    fn civil_decodes_subsecond_and_time() {
        // 2021-06-30 14:23:01.123 UTC.
        // days to 2021-06-30 = 18808; +51781 s; +123 ms.
        let secs = 18_808i64 * 86_400 + 14 * 3_600 + 23 * 60 + 1;
        let us = secs as u64 * 1_000_000 + 123_000;
        let (y, mo, d, hh, mm, ss, ms) = civil(us);
        assert_eq!((y, mo, d, hh, mm, ss, ms), (2021, 6, 30, 14, 23, 1, 123));
    }

    #[test]
    fn filter_threshold_excludes_more_verbose() {
        let mut e = sample();
        e.priority = Priority::Info; // level 6
        let f = Filter {
            min_priority: 3,
            ..Default::default()
        };
        assert!(
            !f.passes(&e),
            "info must be hidden at errors-only threshold"
        );
        e.priority = Priority::Err; // level 3
        assert!(f.passes(&e));
    }

    #[test]
    fn filter_search_is_case_insensitive_over_message_and_unit() {
        let mut e = sample();
        e.message = "Mount FAILED for /var/workspace".into();
        let f = Filter {
            search: "failed".into(),
            ..Default::default()
        };
        assert!(f.passes(&e));
        let f2 = Filter {
            search: "absent".into(),
            ..Default::default()
        };
        assert!(!f2.passes(&e));
    }

    #[test]
    fn unit_short_strips_service_suffix_only() {
        let mut e = sample();
        e.unit = "quick-fs.service".into();
        assert_eq!(e.unit_short(), "quick-fs");
        e.unit = "dbus.socket".into();
        assert_eq!(e.unit_short(), "dbus.socket");
    }

    fn sample() -> LogEntry {
        LogEntry {
            realtime_us: 1_609_459_200_000_000,
            unit: "shell.service".into(),
            identifier: "shell".into(),
            priority: Priority::Info,
            message: "hello".into(),
            pid: Some(1234),
            hostname: "qos".into(),
            boot_id: "boot-a".into(),
            transport: "stdout".into(),
            extra: vec![],
        }
    }
}
