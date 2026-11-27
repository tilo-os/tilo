//! The `journal` resource: this computer's systemd journal as rows, the cursor a live tail
//! continues from, and whether this window follows it.
//!
//! What reads `journalctl` is here, beside the reading's own type: the session's system service
//! reads the journal for the shell, and a device whose daemon has no session (a server, or a
//! capture's disposable profile) reads it for the apps it serves — one read, whoever serves it.
//! The entries are this device's and are never shared: only the filter people agree on lives in
//! the workspace.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const JOURNAL: &str = "journal";

/// One field of a journal record the reading does not promote to a column.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct JournalField {
    pub name: String,
    pub value: String,
}

/// One record of the journal, as the window lists it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct JournalEntry {
    /// The journal's own realtime timestamp, in microseconds since the Unix epoch.
    pub realtime_us: u64,
    /// The unit that wrote it, or the identifier when it belonged to no unit.
    pub unit: String,
    /// The program that wrote it (`SYSLOG_IDENTIFIER`, else `_COMM`).
    pub identifier: String,
    /// The RFC 5424 severity, 0 (emergency) to 7 (debug).
    pub priority: u8,
    /// Its message, without the terminal escapes a program wrote for a console.
    pub message: String,
    pub pid: Option<u32>,
    pub hostname: String,
    pub boot_id: String,
    pub transport: String,
    /// The record's other fields, sorted by name.
    pub extra: Vec<JournalField>,
    /// Its time in the person's zone: `HH:MM:SS.mmm` today, `Mon DD HH:MM:SS.mmm` earlier.
    pub time: String,
    /// Its full local timestamp, `YYYY-MM-DD HH:MM:SS.mmm`.
    pub timestamp: String,
}

/// What this computer's journal reads as now.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Journal {
    /// Whether `journalctl` answers on this machine.
    #[serde(default)]
    pub serving: bool,
    /// Why the journal could not be read, when it could not.
    #[serde(default)]
    pub read_error: Option<String>,
    /// Why the last action did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
    /// The records read, oldest first.
    #[serde(default)]
    pub entries: Vec<JournalEntry>,
    /// Where the next tail read continues from; empty until a read answered.
    #[serde(default)]
    pub cursor: String,
    /// Whether the journal is followed as new records arrive.
    #[serde(default)]
    pub following: bool,
    /// When the journal was last read, in milliseconds since the Unix epoch.
    #[serde(default)]
    pub read_at_ms: u64,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
}

/// The `journal` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.journal")]
pub enum JournalAction {
    /// Read the most recent records.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "refresh journal, journal neu lesen"
    )]
    Refresh {
        #[param("How many records to read")]
        limit: u32,
    },
    /// Read the records written after `cursor`.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "arrow-down",
        keywords = "follow journal, journal folgen"
    )]
    ReadAfter {
        #[param("The cursor to read after")]
        cursor: String,
        #[param("How many records to read")]
        limit: u32,
    },
    /// Follow the journal as new records arrive, or stop following it.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "play", keywords = "follow live, live verfolgen")]
    SetFollowing {
        #[param("Whether to follow new records")]
        on: bool,
    },
}

// ---------------------------------------------------------------- reading this device's journal

/// How many bytes of `journalctl` output one read may hold.
#[cfg(not(target_arch = "wasm32"))]
const OUTPUT_LIMIT: usize = 32 * 1024 * 1024;
/// How long one read waits for `journalctl` before it says the journal cannot be read.
#[cfg(not(target_arch = "wasm32"))]
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
/// The most records one read asks for.
#[cfg(not(target_arch = "wasm32"))]
const MAX_LIMIT: u32 = 5000;

/// The variable a capture's profile sets to the journal it stages: a file of `journalctl -o json`
/// lines, read in place of this machine's journal. Only a capture's disposable profile sets it,
/// so a picture of Logs shows the journal its plan wrote and never the machine's.
#[cfg(not(target_arch = "wasm32"))]
pub const FIXTURE_ENV: &str = "TILO_JOURNAL_FIXTURE";

/// The fields the reading promotes to columns; the rest become [`JournalField`]s.
#[cfg(not(target_arch = "wasm32"))]
const PROMOTED: &[&str] = &[
    "__REALTIME_TIMESTAMP",
    "_SYSTEMD_UNIT",
    "SYSLOG_IDENTIFIER",
    "_COMM",
    "PRIORITY",
    "MESSAGE",
    "_PID",
    "_HOSTNAME",
    "_BOOT_ID",
    "_TRANSPORT",
    "__CURSOR",
    "__MONOTONIC_TIMESTAMP",
];

/// The most recent `limit` records of this computer's journal, oldest first, and the cursor of
/// the last one read.
///
/// # Errors
///
/// Why the journal could not be read.
#[cfg(not(target_arch = "wasm32"))]
pub fn read(limit: u32) -> Result<(Vec<JournalEntry>, String), String> {
    run(None, limit)
}

/// The records written after `cursor`, oldest first, and the cursor of the last one read.
///
/// # Errors
///
/// Why the journal could not be read.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_after(cursor: &str, limit: u32) -> Result<(Vec<JournalEntry>, String), String> {
    run(Some(cursor), limit)
}

#[cfg(not(target_arch = "wasm32"))]
fn run(after: Option<&str>, limit: u32) -> Result<(Vec<JournalEntry>, String), String> {
    if let Some(text) = fixture()? {
        let (entries, cursor) = parse(&text);
        // A staged journal has no records written after it: following reads nothing new.
        return Ok(if after.is_some() {
            (Vec::new(), cursor)
        } else {
            (entries, cursor)
        });
    }
    let count = limit.clamp(1, MAX_LIMIT).to_string();
    let cursor_arg = after.map(|cursor| format!("--after-cursor={cursor}"));
    let mut args: Vec<&str> = vec!["-o", "json", "--no-pager", "-n", &count];
    if let Some(cursor) = &cursor_arg {
        args.push(cursor);
    }
    let output = bounded_command::run("journalctl", &args, None, OUTPUT_LIMIT, TIMEOUT).map_err(
        |error| match error {
            bounded_command::Error::Process(ref io)
                if io.kind() == std::io::ErrorKind::NotFound =>
            {
                "The system journal is not available on this device.".to_owned()
            }
            _ => "The system journal could not be read.".to_owned(),
        },
    )?;
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
    Ok(parse(&String::from_utf8_lossy(&output.stdout)))
}

/// The journal a capture staged, when its profile names one ([`FIXTURE_ENV`]).
///
/// # Errors
///
/// The profile names a journal that cannot be read.
#[cfg(not(target_arch = "wasm32"))]
fn fixture() -> Result<Option<String>, String> {
    let Some(path) = std::env::var_os(FIXTURE_ENV).filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    std::fs::read_to_string(&path)
        .map(Some)
        .map_err(|error| format!("the capture's journal cannot be read: {error}"))
}

/// Parse a newline-delimited `journalctl -o json` stream, oldest first, with the last cursor
/// journalctl wrote.
#[cfg(not(target_arch = "wasm32"))]
fn parse(text: &str) -> (Vec<JournalEntry>, String) {
    let mut cursor = String::new();
    let mut entries: Vec<JournalEntry> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|value| {
            if let Some(at) = field(&value, "__CURSOR") {
                cursor = at;
            }
            entry_of(&value)
        })
        .collect();
    entries.sort_by_key(|entry| entry.realtime_us);
    (entries, cursor)
}

/// One journal record as the reading shows it, when it names a message.
#[cfg(not(target_arch = "wasm32"))]
fn entry_of(value: &serde_json::Value) -> Option<JournalEntry> {
    let object = value.as_object()?;
    let realtime_us = field(value, "__REALTIME_TIMESTAMP")
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(0);
    let identifier = field(value, "SYSLOG_IDENTIFIER")
        .or_else(|| field(value, "_COMM"))
        .unwrap_or_default();
    let unit = field(value, "_SYSTEMD_UNIT")
        .filter(|unit| !unit.is_empty())
        .unwrap_or_else(|| {
            if identifier.is_empty() {
                "(unknown)".to_owned()
            } else {
                identifier.clone()
            }
        });
    let priority = field(value, "PRIORITY")
        .and_then(|level| level.parse::<u8>().ok())
        .unwrap_or(6);
    let message = without_terminal_escapes(&field(value, "MESSAGE").unwrap_or_default());
    let pid = field(value, "_PID").and_then(|pid| pid.parse::<u32>().ok());
    let hostname = field(value, "_HOSTNAME").unwrap_or_default();
    let boot_id = field(value, "_BOOT_ID").unwrap_or_default();
    let transport = field(value, "_TRANSPORT").unwrap_or_default();
    let mut extra: Vec<JournalField> = object
        .iter()
        .filter(|(name, _)| !PROMOTED.contains(&name.as_str()))
        .map(|(name, held)| JournalField {
            name: name.clone(),
            value: field(value, name).unwrap_or_else(|| held.to_string()),
        })
        .collect();
    extra.sort_by(|a, b| a.name.cmp(&b.name));
    let (time, timestamp) = local_strings(realtime_us);
    Some(JournalEntry {
        realtime_us,
        unit,
        identifier,
        priority,
        message,
        pid,
        hostname,
        boot_id,
        transport,
        extra,
        time,
        timestamp,
    })
}

/// Read a journal field that may be a JSON string, an integer, or a byte array (journald
/// encodes non-UTF8 fields as arrays).
#[cfg(not(target_arch = "wasm32"))]
fn field(value: &serde_json::Value, key: &str) -> Option<String> {
    use serde_json::Value;
    match value.get(key)? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Array(items) => {
            let bytes: Vec<u8> = items
                .iter()
                .filter_map(|item| item.as_u64().and_then(|byte| u8::try_from(byte).ok()))
                .collect();
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => None,
    }
}

/// The record's time in the person's zone: `HH:MM:SS.mmm` today, `Mon DD HH:MM:SS.mmm` on an
/// earlier day, and its full `YYYY-MM-DD HH:MM:SS.mmm` timestamp.
#[cfg(not(target_arch = "wasm32"))]
fn local_strings(realtime_us: u64) -> (String, String) {
    let Ok(micros) = i64::try_from(realtime_us) else {
        return (String::new(), String::new());
    };
    let Ok(instant) = jiff::Timestamp::from_microsecond(micros) else {
        return (String::new(), String::new());
    };
    let zone = jiff::tz::TimeZone::system();
    let at = instant.to_zoned(zone.clone());
    let now = jiff::Timestamp::now().to_zoned(zone);
    let timestamp = at.strftime("%Y-%m-%d %H:%M:%S%.3f").to_string();
    let time = if at.date() == now.date() {
        at.strftime("%H:%M:%S%.3f").to_string()
    } else {
        at.strftime("%b %d %H:%M:%S%.3f").to_string()
    };
    (time, timestamp)
}

/// `text` without the terminal's colour and cursor sequences a program wrote for a console
/// (`ESC [ … m`, `ESC ] … BEL`) and without other control characters, which have no glyph:
/// the words alone.
#[cfg(not(target_arch = "wasm32"))]
fn without_terminal_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    for c in chars.by_ref() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => out.push(' '),
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::{entry_of, parse};

    /// What a capture's staged journal is: `journalctl -o json` lines, a record on each, read
    /// oldest first with the last cursor the stream wrote.
    #[test]
    fn a_staged_journal_reads_as_the_records_journalctl_writes() {
        let staged = [
            r#"{"__REALTIME_TIMESTAMP":"1699999990000000","__CURSOR":"s=two","SYSLOG_IDENTIFIER":"kernel","PRIORITY":"6","MESSAGE":"usb 1-3: new device"}"#,
            r#"{"__REALTIME_TIMESTAMP":"1700000000000000","__CURSOR":"s=one","_SYSTEMD_UNIT":"tilo-session.service","SYSLOG_IDENTIFIER":"tilo-session","PRIORITY":"4","MESSAGE":"the GPU lost its context","_PID":42,"_HOSTNAME":"studio","_BOOT_ID":"boot","_TRANSPORT":"journal","TILO_EXTRA":"kept"}"#,
        ]
        .join("\n");
        let (entries, cursor) = parse(&staged);
        assert_eq!(cursor, "s=one", "the cursor the last record wrote");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "usb 1-3: new device", "oldest first");
        assert_eq!(
            entries[0].unit, "kernel",
            "a record with no unit is its program's"
        );
        assert_eq!(entries[1].unit, "tilo-session.service");
        assert_eq!(entries[1].priority, 4);
        assert_eq!(entries[1].pid, Some(42));
        assert!(
            entries[1]
                .extra
                .iter()
                .any(|field| field.name == "TILO_EXTRA"),
            "a field the reading does not promote stays a field"
        );
        assert!(
            entry_of(&serde_json::json!({"MESSAGE": "a record with no timestamp"}))
                .is_some_and(|entry| entry.realtime_us == 0),
            "a record without a timestamp still reads"
        );
    }
}
