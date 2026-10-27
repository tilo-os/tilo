//! What the kernel dropped, read from this device's journal.
//!
//! A firewall UI that cannot show dropped traffic cannot answer the two
//! questions people actually have: "are my deny rules working?" and "what is
//! hammering this machine?". Netfilter logs one record per dropped packet, so
//! the journal is the honest source; no packet is ever inferred.
//!
//! [`read`] runs `journalctl` read-only. It never changes the firewall, never
//! needs the apply capability, and classifies a failed read instead of
//! pretending the window was quiet.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

use ui::time::SystemTime;

/// How many buckets a report's time series has.
pub const BUCKETS: usize = 12;

/// One group of dropped packets: the rule that dropped them, how many, when
/// the last one was seen, and how they were distributed over the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedGroup {
    /// What the kernel did (`DROP` or `REJECT`).
    pub action: String,
    /// `tcp`, `udp`, `icmp`, or empty when the record did not say.
    pub protocol: String,
    /// The destination port, when the record names one.
    pub port: Option<String>,
    /// The interface the packet arrived on, when the record names one.
    pub interface: Option<String>,
    /// How many packets this group accounts for.
    pub packets: u64,
    /// Distinct source addresses seen for this group.
    pub sources: u64,
    /// When the newest packet of this group was logged.
    pub last_seen: SystemTime,
    /// Packets per equal slice of the report window, oldest first.
    pub buckets: Vec<u32>,
    /// The report's whole window, in minutes, so a bucket's length is known.
    pub window_minutes: u32,
}

impl BlockedGroup {
    /// What the rule is, in the person's words: `Drop TCP port 5900`.
    pub fn title(&self) -> String {
        let action = match self.action.as_str() {
            "REJECT" => "Reject",
            _ => "Drop",
        };
        let proto = if self.protocol.is_empty() {
            String::new()
        } else {
            format!("{} ", self.protocol.to_ascii_uppercase())
        };
        match &self.port {
            Some(port) => format!("{action} {proto}port {port}"),
            None if proto.is_empty() => action.to_owned(),
            None => format!("{action} {proto}traffic").trim_end().to_owned(),
        }
    }

    /// The group's source spread, in the person's words.
    pub fn sources_label(&self) -> String {
        match self.sources {
            1 => "from 1 address".to_owned(),
            n => format!("from {n} addresses"),
        }
    }

    /// The window's shape as bars, oldest first. The tallest bucket is full
    /// height; [`Self::trend_scale`] names what a full bar is, so the shape
    /// is a scale and not a decoration.
    pub fn trend(&self) -> String {
        let peak = self.buckets.iter().copied().max().unwrap_or(0);
        if peak == 0 {
            return String::new();
        }
        const BARS: [char; 8] = [
            '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
            '\u{2588}',
        ];
        self.buckets
            .iter()
            .map(|count| {
                let level = (u64::from(*count) * 7 / u64::from(peak)) as usize;
                BARS[level.min(BARS.len() - 1)]
            })
            .collect()
    }

    /// What a full bar means, and over how long a bucket.
    pub fn trend_scale(&self) -> String {
        let peak = self.buckets.iter().copied().max().unwrap_or(0);
        let minutes = (self.window_minutes / BUCKETS as u32).max(1);
        format!("{peak} per {minutes} min")
    }
}

/// One read of dropped traffic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedReport {
    /// The groups, busiest first.
    pub groups: Vec<BlockedGroup>,
    /// How many packets all groups account for.
    pub total: u64,
    /// When the read happened.
    pub read_at: SystemTime,
    /// The window the read covers, in minutes.
    pub window_minutes: u32,
    /// The journal had records that are not netfilter drops, so the read is
    /// partial; the person is told rather than shown a confident zero.
    pub note: Option<String>,
}

impl BlockedReport {
    /// A report with nothing dropped in its window. This is a measured zero,
    /// unlike a failed read.
    pub fn empty(read_at: SystemTime, window_minutes: u32) -> Self {
        Self {
            groups: Vec::new(),
            total: 0,
            read_at,
            window_minutes,
            note: None,
        }
    }
}

/// Read `window` of kernel drops from this device's journal.
///
/// # Errors
///
/// A sentence safe to show: the journal is unavailable, permission was
/// denied, or the read failed. The command's own stderr is never relayed.
pub fn read(window: Duration) -> Result<BlockedReport, String> {
    let minutes = u32::try_from(window.as_secs() / 60).unwrap_or(60).max(1);
    let since = format!("-{minutes} min");
    let output = bounded_command::run(
        "journalctl",
        &["--kernel", "--no-pager", "-o", "json", "--since", &since],
        None,
        16 * 1024 * 1024,
        Duration::from_secs(8),
    )
    .map_err(|error| match error {
        bounded_command::Error::Process(ref io_error)
            if io_error.kind() == std::io::ErrorKind::NotFound =>
        {
            "The system journal is not available on this device.".to_owned()
        }
        _ => "The system journal could not be read.".to_owned(),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        return Err(
            if stderr.contains("permission") || stderr.contains("access denied") {
                "This session is not allowed to read the system journal.".to_owned()
            } else {
                "The system journal could not be read.".to_owned()
            },
        );
    }
    Ok(parse(&String::from_utf8_lossy(&output.stdout), minutes))
}

/// Parse `journalctl -o json` lines into a report. Pure, so the grouping and
/// time buckets are testable against captured records.
pub fn parse(text: &str, window_minutes: u32) -> BlockedReport {
    let now = ui::time::wall();
    let window = Duration::from_secs(u64::from(window_minutes) * 60);
    let start = now.checked_sub(window).unwrap_or(now);
    let bucket = (window.as_secs() / BUCKETS as u64).max(1);
    let mut groups: BTreeMap<(String, String, Option<String>, Option<String>), Group> =
        BTreeMap::new();
    let mut skipped = false;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(message) = value.get("MESSAGE").and_then(Value::as_str) else {
            continue;
        };
        let Some(drop) = parse_drop(message) else {
            continue;
        };
        let at = value
            .get("__REALTIME_TIMESTAMP")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|us| ui::time::UNIX_EPOCH.checked_add(Duration::from_micros(us)))
            .unwrap_or(now);
        let offset = at.duration_since(start).unwrap_or(window).as_secs();
        let index = usize::try_from(offset / bucket)
            .unwrap_or(BUCKETS)
            .min(BUCKETS - 1);
        let entry = groups
            .entry((
                drop.action.clone(),
                drop.protocol.clone(),
                drop.port.clone(),
                drop.interface.clone(),
            ))
            .or_insert_with(Group::default);
        entry.packets += 1;
        entry.last_seen = entry.last_seen.max(at);
        entry.buckets[index] = entry.buckets[index].saturating_add(1);
        if let Some(source) = drop.source {
            entry.sources.insert(source);
        } else {
            skipped = true;
        }
    }
    let mut groups: Vec<BlockedGroup> = groups
        .into_iter()
        .map(
            |((action, protocol, port, interface), group)| BlockedGroup {
                action,
                protocol,
                port,
                interface,
                packets: group.packets,
                sources: u64::try_from(group.sources.len()).unwrap_or(u64::MAX),
                last_seen: group.last_seen,
                buckets: group.buckets,
                window_minutes,
            },
        )
        .collect();
    groups.sort_by(|a, b| {
        b.packets
            .cmp(&a.packets)
            .then_with(|| a.title().cmp(&b.title()))
    });
    let total = groups.iter().map(|group| group.packets).sum();
    BlockedReport {
        groups,
        total,
        read_at: now,
        window_minutes,
        note: skipped.then(|| {
            "Some journal records did not name a source address; they are counted without one."
                .to_owned()
        }),
    }
}

struct Group {
    packets: u64,
    sources: std::collections::BTreeSet<String>,
    buckets: Vec<u32>,
    last_seen: SystemTime,
}

impl Default for Group {
    fn default() -> Self {
        Self {
            packets: 0,
            sources: std::collections::BTreeSet::new(),
            buckets: vec![0; BUCKETS],
            last_seen: ui::time::UNIX_EPOCH,
        }
    }
}

struct Drop {
    action: String,
    protocol: String,
    port: Option<String>,
    interface: Option<String>,
    source: Option<String>,
}

/// Read netfilter's `key=value` record, when `message` is one.
fn parse_drop(message: &str) -> Option<Drop> {
    // Netfilter's LOG record may or may not spell `DROP=1`; the stable
    // shape is the packet's own fields. A record with no source or packet
    // field is ordinary kernel output, not a drop.
    let looks_like_packet = (message.contains("PROTO=") || message.contains("DPT="))
        && (message.contains("SRC=")
            || message.contains("DROP=")
            || message.contains("REJECT=")
            || message.contains("IN=")
            || message.contains("OUT="));
    if !looks_like_packet {
        return None;
    }
    let field = |name: &str| {
        message
            .split_whitespace()
            .find_map(|part| part.strip_prefix(name)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let action = if field("REJECT").is_some() {
        "REJECT"
    } else {
        "DROP"
    };
    Some(Drop {
        action: action.to_owned(),
        protocol: field("PROTO").unwrap_or_default().to_ascii_lowercase(),
        port: field("DPT"),
        interface: field("IN"),
        source: field("SRC"),
    })
}

/// A representative report for a capture, laid out from the pinned clock.
/// It is never shown in a live window: the live app reads the journal.
pub fn sample() -> BlockedReport {
    let now = ui::time::wall();
    let group = |action: &str,
                 protocol: &str,
                 port: Option<&str>,
                 packets: u64,
                 sources: u64,
                 buckets: [u32; BUCKETS],
                 minutes_ago: u64| BlockedGroup {
        action: action.to_owned(),
        protocol: protocol.to_owned(),
        port: port.map(str::to_owned),
        interface: Some("wlan0".to_owned()),
        packets,
        sources,
        last_seen: now
            .checked_sub(Duration::from_secs(minutes_ago * 60))
            .unwrap_or(now),
        buckets: buckets.to_vec(),
        window_minutes: 60,
    };
    let groups = vec![
        group(
            "DROP",
            "tcp",
            Some("5900"),
            184,
            23,
            [18, 29, 41, 27, 12, 9, 20, 28, 0, 0, 0, 0],
            2,
        ),
        group(
            "REJECT",
            "tcp",
            Some("23"),
            41,
            4,
            [0, 0, 3, 2, 7, 8, 11, 10, 0, 0, 0, 0],
            11,
        ),
        group(
            "DROP",
            "udp",
            Some("1900"),
            12,
            1,
            [1, 2, 0, 0, 1, 3, 2, 3, 0, 0, 0, 0],
            24,
        ),
    ];
    BlockedReport {
        total: groups.iter().map(|group| group.packets).sum(),
        groups,
        read_at: now,
        window_minutes: 60,
        note: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDS: &str = r#"{"MESSAGE":"IN=wlan0 OUT= MAC=00:11 SRC=203.0.113.7 DST=192.168.1.4 LEN=60 PROTO=TCP SPT=51234 DPT=5900 WINDOW=65535 SYN","__REALTIME_TIMESTAMP":"1792918422528049"}
{"MESSAGE":"IN=wlan0 OUT= MAC=00:11 SRC=203.0.113.8 DST=192.168.1.4 LEN=60 PROTO=TCP SPT=51235 DPT=5900 WINDOW=65535 SYN","__REALTIME_TIMESTAMP":"1792918423528049"}
{"MESSAGE":"IN=wlan0 OUT= MAC=00:11 SRC=198.51.100.2 DST=192.168.1.4 LEN=84 PROTO=ICMP TYPE=8 DROP=1","__REALTIME_TIMESTAMP":"1792918424528049"}
{"MESSAGE":"ordinary kernel message","__REALTIME_TIMESTAMP":"1792918425528049"}"#;

    #[test]
    fn drops_group_by_rule_and_count_their_sources() {
        let report = parse(RECORDS, 60);
        assert_eq!(report.total, 3);
        assert_eq!(report.groups.len(), 2);
        let tcp = &report.groups[0];
        assert_eq!(tcp.action, "DROP");
        assert_eq!(tcp.protocol, "tcp");
        assert_eq!(tcp.port.as_deref(), Some("5900"));
        assert_eq!(tcp.packets, 2);
        assert_eq!(tcp.sources, 2);
        assert_eq!(tcp.title(), "Drop TCP port 5900");
        assert_eq!(tcp.sources_label(), "from 2 addresses");
        assert_eq!(tcp.buckets.iter().sum::<u32>(), 2);
    }

    #[test]
    fn a_record_without_a_source_is_counted_and_disclosed() {
        let report = parse(
            r#"{"MESSAGE":"IN=wlan0 OUT= MAC=00:11 LEN=60 PROTO=TCP DPT=22 DROP=1","__REALTIME_TIMESTAMP":"1792918422528049"}"#,
            60,
        );
        assert_eq!(report.total, 1);
        assert!(report.note.is_some());
    }

    #[test]
    fn an_empty_window_is_a_measured_zero_with_no_groups() {
        let report = parse("{\"MESSAGE\":\"ordinary message\"}", 15);
        assert_eq!(report.total, 0);
        assert!(report.groups.is_empty());
        assert!(report.note.is_none());
    }
}
