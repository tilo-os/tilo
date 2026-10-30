//! The computer itself stopping, as the kernel recorded it.
//!
//! The kernel keeps its log of a panic or oops in persistent memory across the
//! reset that follows (pstore, a ramoops region on boards that reserve one).
//! At the next start [`take_in`] reads those records, from `/sys/fs/pstore`
//! where this account can read it and from this boot's journal, where
//! `systemd-pstore` moves them before any session starts, and keeps each as a
//! `…-stopped.log` record beside the session's own. A record already kept is
//! known by the text's digest, so reading the same ones at every start keeps
//! each once. The kernel keeps an oops too, a fault it ends the faulting task
//! for and runs on from: its record is a fault of the system's core, never the
//! computer stopping.

use std::io::Read as _;
use std::path::Path;

use chrono::TimeZone as _;
use tilo_actions::remote::crash_records::StopReason;

use super::FILE_TIMESTAMP;
use super::records::STOPPED_SUFFIX;

/// The heading of a record of the computer stopping.
pub const STOPPED_TITLE: &str = "tilo computer stopped";

/// The heading of a record of a fault the system's core went on from.
pub const FAULT_TITLE: &str = "tilo system fault";

/// The most of one persistent-memory record read at the start.
const MAX_PSTORE_ENTRY: u64 = 1024 * 1024;

/// The records the kernel keeps.
const PSTORE_DIR: &str = "/sys/fs/pstore";

/// One record the kernel kept in persistent memory: its name there
/// (`dmesg-ramoops-0`), what it holds, and when it was written, if known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PstoreEntry {
    pub name: String,
    pub content: Vec<u8>,
    /// Seconds since the Unix epoch.
    pub written: Option<i64>,
}

/// The computer stopping, as the kernel recorded it: why, when if known,
/// and its log up to then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stop {
    pub reason: StopReason,
    pub written: Option<i64>,
    pub log: String,
}

const fn reason_word(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Panic => "panic",
        StopReason::Oops => "oops",
        StopReason::Emergency => "emergency",
    }
}

/// The reason a record names, as the kernel writes it.
fn reason_named(name: &str) -> Option<StopReason> {
    match name.trim().to_ascii_lowercase().as_str() {
        "panic" => Some(StopReason::Panic),
        "oops" => Some(StopReason::Oops),
        "emergency" => Some(StopReason::Emergency),
        _ => None,
    }
}

/// The kernel's heading of one part of a log it dumped (`Panic#1 Part1`):
/// why, which dump of the boot and which part, the newest part first. A
/// dump at a clean shutdown is no stop.
fn dump_heading(first_line: &str) -> Option<(StopReason, u32, u32)> {
    let (why, rest) = first_line.trim().split_once('#')?;
    let (count, part) = rest.split_once(" Part")?;
    Some((
        reason_named(why)?,
        count.trim().parse().ok()?,
        part.trim().parse().ok()?,
    ))
}

/// The stops `entries` record: each dumped log, its parts joined oldest
/// first. The console log and other records the kernel keeps beside them
/// are not stops of their own.
#[must_use]
pub fn stops(entries: &[PstoreEntry]) -> Vec<Stop> {
    type Part<'a> = (u32, &'a str, Option<i64>);
    let texts: Vec<(String, &PstoreEntry)> = entries
        .iter()
        .filter(|entry| entry.name.starts_with("dmesg-") && !entry.name.ends_with(".enc.z"))
        .map(|entry| (String::from_utf8_lossy(&entry.content).into_owned(), entry))
        .collect();
    let mut dumps: Vec<((StopReason, u32), Vec<Part<'_>>)> = Vec::new();
    for (text, entry) in &texts {
        let (first, body) = text.split_once('\n').unwrap_or((text.as_str(), ""));
        let Some((reason, count, part)) = dump_heading(first) else {
            continue;
        };
        let key = (reason, count);
        let part = (part, body, entry.written);
        match dumps.iter_mut().find(|(held, _)| *held == key) {
            Some((_, parts)) => parts.push(part),
            None => dumps.push((key, vec![part])),
        }
    }
    dumps
        .into_iter()
        .map(|((reason, _), mut parts)| {
            parts.sort_by_key(|part| part.0);
            Stop {
                reason,
                written: parts.iter().filter_map(|part| part.2).max(),
                log: parts
                    .iter()
                    .map(|part| part.1.trim_end_matches('\n'))
                    .collect::<Vec<_>>()
                    .join("\n"),
            }
        })
        .collect()
}

/// The records in the persistent-memory filesystem `dir`, as far as this
/// account may read them.
#[must_use]
pub fn read_pstore_dir(dir: &Path) -> Vec<PstoreEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            if !entry.file_type().ok()?.is_file() {
                return None;
            }
            let file = std::fs::File::open(entry.path()).ok()?;
            // The kernel writes the dump's own time as the file's.
            let written = file
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|since| i64::try_from(since.as_secs()).ok())
                .filter(|secs| *secs > 0);
            let mut content = Vec::new();
            file.take(MAX_PSTORE_ENTRY).read_to_end(&mut content).ok()?;
            Some(PstoreEntry {
                name,
                content,
                written,
            })
        })
        .collect()
}

/// The bytes a journal field holds in `journalctl -o json`: a string, or an
/// array of bytes for one that is not text.
fn journal_bytes(value: &serde_json::Value) -> Option<Vec<u8>> {
    match value {
        serde_json::Value::String(text) => Some(text.as_bytes().to_vec()),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| item.as_u64().and_then(|byte| u8::try_from(byte).ok()))
            .collect(),
        _ => None,
    }
}

/// The records `systemd-pstore` moved into the journal, from the lines of
/// `journalctl -o json`: each names the record in its message
/// (`PStore dmesg-ramoops-0.`) and carries it as its `FILE` field. The
/// journal keeps no time of the dump itself.
#[must_use]
pub fn journal_entries(json_lines: &str) -> Vec<PstoreEntry> {
    json_lines
        .lines()
        .filter_map(|line| {
            let entry: serde_json::Value = serde_json::from_str(line).ok()?;
            let message = entry.get("MESSAGE")?.as_str()?;
            let named = message.strip_prefix("PStore ")?;
            let name = named
                .split_once(" moved to ")
                .map_or(named, |(name, _)| name)
                .trim_end_matches('.');
            Some(PstoreEntry {
                name: name.to_owned(),
                content: journal_bytes(entry.get("FILE")?)?,
                written: None,
            })
        })
        .collect()
}

/// The records `systemd-pstore` moved into this boot's journal, when this
/// account may read the system journal.
#[cfg(target_os = "linux")]
fn read_journal() -> Vec<PstoreEntry> {
    const JOURNALCTL: &str = "/usr/bin/journalctl";
    if !Path::new(JOURNALCTL).exists() {
        return Vec::new();
    }
    let output = bounded_command::run(
        JOURNALCTL,
        &[
            "-b",
            "0",
            "-q",
            "--no-pager",
            "-o",
            "json",
            "--all",
            "--output-fields=MESSAGE,FILE",
            "_SYSTEMD_UNIT=systemd-pstore.service",
        ],
        None,
        8 * 1024 * 1024,
        std::time::Duration::from_secs(5),
    );
    match output {
        Ok(output) => journal_entries(&String::from_utf8_lossy(&output.stdout)),
        Err(error) => {
            tracing::debug!(%error, "the journal's records of the computer stopping are unreadable");
            Vec::new()
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn read_journal() -> Vec<PstoreEntry> {
    Vec::new()
}

/// The record kept for `stop`, written as `stamp` (local time, the crash
/// log's file-name form).
#[must_use]
pub fn render_stop(stop: &Stop, stamp: &str) -> String {
    let (title, outcome) = if stop.reason == StopReason::Oops {
        (
            FAULT_TITLE,
            "the system ended the faulting task and went on running",
        )
    } else {
        (STOPPED_TITLE, "the computer stopped and started again")
    };
    let rule = "=".repeat(title.len());
    let when = if stop.written.is_some() {
        "when the system recorded it"
    } else {
        "the start that found it"
    };
    format!(
        "{title}\n{rule}\n\
         timestamp: {stamp}\n\
         time-of:   {when}\n\
         reason:    {}\n\
         outcome:   {outcome}\n\n\
         {}\n",
        reason_word(stop.reason),
        stop.log
    )
}

/// The name a kept stop ends with: its text's digest, which knows it again.
fn stop_tail(stop: &Stop) -> String {
    let digest = blake3::hash(stop.log.as_bytes()).to_hex();
    let short = digest.as_str().get(..16).unwrap_or_default();
    format!("-{short}{STOPPED_SUFFIX}")
}

/// Keep each of `stops` not already kept as a record in `dir`, timed when
/// the system recorded it or else `found_at` (seconds since the Unix epoch).
/// The names written.
///
/// # Errors
///
/// The directory could not be read; a record that could not be written is
/// left for the next start.
pub fn keep_stops(dir: &Path, stops: &[Stop], found_at: i64) -> std::io::Result<Vec<String>> {
    let mut held: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    let mut written = Vec::new();
    for stop in stops {
        let tail = stop_tail(stop);
        if held.iter().any(|name| name.ends_with(&tail)) {
            continue;
        }
        let at = stop.written.unwrap_or(found_at);
        let Some(when) = chrono::Local.timestamp_opt(at, 0).earliest() else {
            continue;
        };
        // format: machine (the record's file name)
        let stamp = when.format(FILE_TIMESTAMP).to_string();
        let name = format!("{stamp}{tail}");
        match super::write_record(dir, &name, &render_stop(stop, &stamp)) {
            Ok(_) => {
                held.push(name.clone());
                written.push(name);
            }
            Err(error) => tracing::warn!(%error, "a record of the computer stopping was not kept"),
        }
    }
    Ok(written)
}

/// Keep the records of the computer stopping that the kernel left for this
/// start in `dir`, from every source, and say how many were new.
pub fn take_in(dir: &Path) -> usize {
    // Each source's dumps are joined on their own: a dump numbered alike in
    // both is two dumps.
    let mut found = stops(&read_pstore_dir(Path::new(PSTORE_DIR)));
    found.extend(stops(&read_journal()));
    if found.is_empty() {
        return 0;
    }
    match keep_stops(dir, &found, chrono::Local::now().timestamp()) {
        Ok(written) => written.len(),
        Err(error) => {
            tracing::warn!(%error, "the records of the computer stopping were not kept");
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, content: &str, written: Option<i64>) -> PstoreEntry {
        PstoreEntry {
            name: name.to_owned(),
            content: content.as_bytes().to_vec(),
            written,
        }
    }

    #[test]
    fn a_dumps_parts_are_joined_oldest_first_and_a_console_log_is_no_stop() {
        let entries = [
            entry("dmesg-ramoops-0", "Panic#1 Part2\nsecond half\n", None),
            entry("dmesg-ramoops-1", "Panic#1 Part1\nfirst half\n", Some(17)),
            entry("console-ramoops-0", "just a console log\n", None),
            entry(
                "dmesg-ramoops-2",
                "Panic#2 Part1\nthe next stop\n",
                Some(23),
            ),
            entry("dmesg-ramoops-3", "not a dump heading\n", None),
        ];
        let stops = stops(&entries);
        assert_eq!(stops.len(), 2);
        assert_eq!(stops[0].reason, StopReason::Panic);
        assert_eq!(stops[0].log, "first half\nsecond half");
        assert_eq!(stops[0].written, Some(17));
        assert_eq!(stops[1].log, "the next stop");
    }

    #[test]
    fn a_clean_shutdown_dump_is_no_stop() {
        assert!(stops(&[entry("dmesg-ramoops-0", "Something else\nlog", None)]).is_empty());
    }

    #[test]
    fn the_journals_records_are_read_from_the_messages_that_carried_them() {
        let lines = concat!(
            r#"{"MESSAGE":"PStore dmesg-ramoops-0 moved to /var/lib/systemd/pstore/0/","FILE":[80,97,110,105,99]}"#,
            "\n",
            r#"{"MESSAGE":"PStore dmesg-ramoops-0.","FILE":"Panic#1 Part1\ntext"}"#,
            "\n",
            r#"{"MESSAGE":"Started pstore.service."}"#,
        );
        let entries = journal_entries(lines);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "dmesg-ramoops-0");
        assert_eq!(entries[0].content, b"Panic");
        assert_eq!(entries[1].content, b"Panic#1 Part1\ntext");
        assert_eq!(stops(&entries).len(), 1);
    }

    #[test]
    fn a_stop_is_kept_once_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        tilo_files::native::private::directory(dir.path()).unwrap();
        let stop = Stop {
            reason: StopReason::Emergency,
            written: Some(1_790_000_000),
            log: "sysrq triggered".to_owned(),
        };
        let first = keep_stops(dir.path(), &[stop.clone()], 1_790_000_100).unwrap();
        assert_eq!(first.len(), 1);
        assert!(first[0].ends_with(STOPPED_SUFFIX), "{first:?}");
        assert!(
            keep_stops(dir.path(), &[stop], 1_790_000_200)
                .unwrap()
                .is_empty()
        );
        let text = std::fs::read_to_string(dir.path().join(&first[0])).unwrap();
        assert!(text.starts_with(STOPPED_TITLE), "{text}");
        assert!(text.contains("reason:    emergency\n"), "{text}");
        assert!(text.contains("sysrq triggered\n"), "{text}");
    }

    #[test]
    fn a_fault_kept_as_a_record_says_the_system_ran_on() {
        let record = render_stop(
            &Stop {
                reason: StopReason::Oops,
                written: None,
                log: "BUG: kernel page fault".to_owned(),
            },
            "2026-10-24-225201",
        );
        assert!(record.starts_with(FAULT_TITLE), "{record}");
        assert!(
            record.contains("time-of:   the start that found it\n"),
            "{record}"
        );
        assert!(record.contains("went on running"), "{record}");
    }
}
