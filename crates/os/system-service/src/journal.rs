//! The `journal` resource: what [`tilo_system_resources::journal`] says it is.
//!
//! [`read`] reads the most recent records of this computer's systemd journal, [`read_after`]
//! the records written since a cursor, and [`JournalAction`] offers both to a shell. A machine
//! whose journal cannot be read says so; it never answers with an empty journal.

use std::time::Duration;

use serde_json::Value;
use tilo_actions::ResourceSpec;
use tilo_service::offer;
use tilo_system_resources::journal::{JOURNAL, Journal, JournalAction, JournalEntry, JournalField};

use crate::i18n::name;

/// How many bytes of `journalctl` output one read may hold.
const OUTPUT_LIMIT: usize = 32 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(8);
/// The most records one read asks for.
const MAX_LIMIT: u32 = 5000;

/// The fields the reading promotes to columns; the rest become [`JournalField`]s.
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

/// The `journal` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.journal", label = "System journal")]
pub struct JournalReading;

/// The `journal` resource reading `journal`.
#[must_use]
pub fn resource(_journal: &Journal) -> ResourceSpec {
    offer::<JournalAction>(tilo_resource::typed_resource::<JournalReading>(
        JOURNAL,
        name("so.tilo.system.journal", "System journal"),
    ))
}

/// The `journal` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Journal, JournalAction>(JOURNAL, "System journal")
}

/// The most recent `limit` records, oldest first, and the cursor of the last one read.
///
/// # Errors
/// Why the journal could not be read.
pub fn read(limit: u32) -> Result<(Vec<JournalEntry>, String), String> {
    run(None, limit)
}

/// The records written after `cursor`, oldest first, and the cursor of the last one read.
///
/// # Errors
/// Why the journal could not be read.
pub fn read_after(cursor: &str, limit: u32) -> Result<(Vec<JournalEntry>, String), String> {
    run(Some(cursor), limit)
}

fn run(after: Option<&str>, limit: u32) -> Result<(Vec<JournalEntry>, String), String> {
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

/// Parse a newline-delimited `journalctl -o json` stream, oldest first, with the last cursor
/// journalctl wrote.
fn parse(text: &str) -> (Vec<JournalEntry>, String) {
    let mut cursor = String::new();
    let mut entries: Vec<JournalEntry> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
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
fn entry_of(value: &Value) -> Option<JournalEntry> {
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
fn field(value: &Value, key: &str) -> Option<String> {
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

/// What `limit` means for a read that names none.
#[must_use]
pub const fn default_limit() -> u32 {
    500
}

/// The moment now, in milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
