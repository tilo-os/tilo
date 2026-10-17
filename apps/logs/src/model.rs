//! Pure data model for the Logs viewer — syslog priorities, a single
//! journal entry, and the filter predicate. No rendering, no IO.
//!
//! A [`LogEntry`] is a flattened view of one `journalctl -o json` record:
//! the handful of fields the UI promotes to columns plus every remaining
//! field kept verbatim in [`LogEntry::extra`] for the detail pane.

use ui::prelude::{Model, tilo_resource};
use ui::widget::Tone;

/// The eight syslog severities, ordered most-severe (`Emerg`, level 0) to
/// least-severe (`Debug`, level 7). Level numbers match RFC 5424 and the
/// integer in the journal's `PRIORITY` field. As a filter threshold, the
/// default `Debug` shows everything.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, PartialOrd, Ord, serde::Serialize, Model)]
pub enum Priority {
    Emerg,
    Alert,
    Crit,
    Err,
    Warning,
    Notice,
    Info,
    #[default]
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

    /// The severity's name, the same in the list and the detail.
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

    /// The status tone a severity is marked with: errors and worse are
    /// danger, warnings are warnings, notices are information, and the
    /// verbose tail carries no status.
    pub fn tone(self) -> Tone {
        match self {
            Priority::Emerg | Priority::Alert | Priority::Crit | Priority::Err => Tone::Danger,
            Priority::Warning => Tone::Warning,
            Priority::Notice => Tone::Info,
            Priority::Info | Priority::Debug => Tone::Neutral,
        }
    }

    /// The threshold this severity sets, as the severity filter names it.
    pub fn threshold_label(self) -> &'static str {
        match self {
            Priority::Emerg => "Emergency only",
            Priority::Alert => "Alert and above",
            Priority::Crit => "Critical and above",
            Priority::Err => "Error and above",
            Priority::Warning => "Warning and above",
            Priority::Notice => "Notice and above",
            Priority::Info => "Info and above",
            Priority::Debug => "All levels",
        }
    }
}

/// One flattened journal record.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
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
    /// The list-column time in local time: `HH:MM:SS.mmm` for today,
    /// `Mon DD HH:MM:SS.mmm` for an earlier day.
    pub fn list_time(&self) -> String {
        let (y, mo, d, hh, mm, ss, milli) = civil(local_us(self.realtime_us));
        let now = ui::clock::local_now();
        let now_us = u64::try_from(now.timestamp_micros()).unwrap_or(0);
        let (ty, tmo, td, ..) = civil(local_us(now_us));
        if (ty, tmo, td) == (y, mo, d) {
            format!("{hh:02}:{mm:02}:{ss:02}.{milli:03}")
        } else {
            format!(
                "{} {d:02} {hh:02}:{mm:02}:{ss:02}.{milli:03}",
                MONTHS.get((mo as usize).saturating_sub(1)).unwrap_or(&"")
            )
        }
    }

    /// `YYYY-MM-DD HH:MM:SS.mmm` in local time — the detail timestamp.
    pub fn timestamp_full(&self) -> String {
        let (y, mo, d, hh, mm, ss, milli) = civil(local_us(self.realtime_us));
        format!("{y:04}-{mo:02}-{d:02} {hh:02}:{mm:02}:{ss:02}.{milli:03}")
    }

    /// `HH:MM:SS` in local time, for compact contexts.
    pub fn time_hms(&self) -> String {
        let (_, _, _, hh, mm, ss, _) = civil(local_us(self.realtime_us));
        format!("{hh:02}:{mm:02}:{ss:02}")
    }

    /// The unit label shown in the list — strips a trailing `.service`
    /// for density, leaves other suffixes (`.socket`, `.timer`) intact.
    pub fn unit_short(&self) -> &str {
        self.unit.strip_suffix(".service").unwrap_or(&self.unit)
    }
}

/// `realtime_us` shifted into the local time zone, read through
/// [`ui::clock`] so captures show the same wall clock on every run.
fn local_us(realtime_us: u64) -> u64 {
    let offset_s = i64::from(ui::clock::local_now().offset().local_minus_utc());
    realtime_us.saturating_add_signed(offset_s.saturating_mul(1_000_000))
}

/// The process-local live filter applied to the entry list. Pure predicate
/// logic lives in [`Filter::passes`].
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
/// sec, milli)` tuple in UTC, through `ui::date` so the conversion needs no
/// date crate and stays deterministic offline.
fn civil(realtime_us: u64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let secs = (realtime_us / 1_000_000) as i64;
    let milli = ((realtime_us / 1_000) % 1_000) as u32;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hh = (rem / 3_600) as u32;
    let mm = ((rem % 3_600) / 60) as u32;
    let ss = (rem % 60) as u32;

    let (year, m, d) = ui::date::civil_from_days(days);
    (year, m, d, hh, mm, ss, milli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_round_trips_through_level_and_clamps_out_of_range() {
        for p in Priority::ALL {
            assert_eq!(Priority::from_level(p.level()), p, "case {p:?}");
        }
        // Out-of-range levels clamp to Debug.
        for level in [42, -1] {
            assert_eq!(
                Priority::from_level(level),
                Priority::Debug,
                "case level {level}"
            );
        }
    }

    #[test]
    fn civil_decodes_known_instants_with_subseconds() {
        // 2021-06-30 14:23:01.123 UTC.
        // days to 2021-06-30 = 18808; +51781 s; +123 ms.
        let secs = 18_808i64 * 86_400 + 14 * 3_600 + 23 * 60 + 1;
        let cases = [
            // 2021-01-01 00:00:00.000 UTC = 1609459200 s.
            (
                "new year 2021",
                1_609_459_200_000_000,
                (2021, 1, 1, 0, 0, 0, 0),
            ),
            (
                "mid-2021 with millis",
                secs as u64 * 1_000_000 + 123_000,
                (2021, 6, 30, 14, 23, 1, 123),
            ),
        ];
        for (name, us, want) in cases {
            assert_eq!(civil(us), want, "case {name}");
        }
    }

    #[test]
    fn entry_filters_and_unit_labels() {
        // The priority threshold excludes more verbose entries.
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

        // Search is case-insensitive over message and unit.
        let mut e = sample();
        e.message = "Mount FAILED for /var/workspace".into();
        for (search, passes) in [("failed", true), ("absent", false)] {
            let f = Filter {
                search: search.into(),
                ..Default::default()
            };
            assert_eq!(f.passes(&e), passes, "case search {search:?}");
        }

        // unit_short strips the .service suffix only.
        let mut e = sample();
        for (unit, short) in [
            ("quick-fs.service", "quick-fs"),
            ("dbus.socket", "dbus.socket"),
        ] {
            e.unit = unit.into();
            assert_eq!(e.unit_short(), short, "case unit {unit}");
        }
    }

    fn sample() -> LogEntry {
        LogEntry {
            realtime_us: 1_609_459_200_000_000,
            unit: "shell.service".into(),
            identifier: "shell".into(),
            priority: Priority::Info,
            message: "hello".into(),
            pid: Some(1234),
            hostname: "tilo".into(),
            boot_id: "boot-a".into(),
            transport: "stdout".into(),
            extra: vec![],
        }
    }
}
