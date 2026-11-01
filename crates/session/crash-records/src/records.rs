//! Every record of what went wrong on this computer, listed and read as the
//! facts a person, Settings or an agent asks for.
//!
//! The private crash directory ([`super::directory`]) holds a file for each
//! time the session panicked (`<timestamp>-<pid>.log`), stopped responding
//! (`…-freeze.log`) or found the computer itself had stopped
//! (`…-stopped.log`, written from the kernel's record by [`super::stopped`]).
//! [`list`] says what each record is, newest first and without its text;
//! [`read`] returns one record's text, stacks included, only when it is asked
//! for by name. Nothing here sends a record anywhere.

use std::io::Read as _;
use std::path::Path;

use chrono::{Local, TimeZone as _};
use tilo_actions::remote::crash_records::{CrashKind, CrashRecord, CrashRecordText, StopReason};

use super::{FILE_TIMESTAMP, FREEZE_SUFFIX};

/// The end of the name of a record of the computer stopping.
pub const STOPPED_SUFFIX: &str = "-stopped.log";

/// How much of a record [`list`] reads: its facts, never its stacks.
const HEAD_BYTES: u64 = 4 * 1024;
/// The most of one record [`read`] returns.
pub const MAX_TEXT: u64 = 1024 * 1024;
/// The most records [`list`] answers, newest first.
const MAX_LISTED: usize = 200;

/// `2026-10-24-225201`: how long [`FILE_TIMESTAMP`] renders, and so the
/// prefix of every record's name.
const FILE_TIMESTAMP_LEN: usize = 17;

/// The local time a record's file name starts with.
#[must_use]
pub fn written_at(name: &str) -> Option<chrono::NaiveDateTime> {
    let stamp = name.get(..FILE_TIMESTAMP_LEN)?;
    chrono::NaiveDateTime::parse_from_str(stamp, FILE_TIMESTAMP).ok()
}

/// Whether `name` is a record's: a file name the crash log writes.
#[must_use]
pub fn is_record_name(name: &str) -> bool {
    !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && Path::new(name).extension() == Some("log".as_ref())
        && written_at(name).is_some()
}

/// The value of the header field `key` (`version:   0.1.0`), from the facts
/// a record starts with, up to its first blank line. A placeholder the crash
/// log writes for a fact it could not read (`<none>`) is no value.
fn field<'a>(head: &'a str, key: &str) -> Option<&'a str> {
    head.lines()
        .take_while(|line| !line.trim().is_empty())
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.starts_with('<'))
}

/// The reason a record names, as the crash log and the kernel write it.
fn reason_named(name: &str) -> Option<StopReason> {
    match name.trim().to_ascii_lowercase().as_str() {
        "panic" => Some(StopReason::Panic),
        "oops" => Some(StopReason::Oops),
        "emergency" => Some(StopReason::Emergency),
        _ => None,
    }
}

/// What the record `name`, starting with `head`, says happened. `None` for
/// a name the crash log does not write.
#[must_use]
pub fn summarize(name: &str, head: &str) -> Option<CrashRecord> {
    if !is_record_name(name) {
        return None;
    }
    let at = Local
        .from_local_datetime(&written_at(name)?)
        .earliest()?
        .timestamp();
    let reason = name
        .ends_with(STOPPED_SUFFIX)
        .then(|| field(head, "reason").and_then(reason_named))
        .flatten();
    let kind = if name.ends_with(FREEZE_SUFFIX) {
        CrashKind::Froze
    } else if name.ends_with(STOPPED_SUFFIX) {
        // An oops ended a task and the system ran on: nothing stopped.
        if reason == Some(StopReason::Oops) {
            CrashKind::SystemFault
        } else {
            CrashKind::ComputerStopped
        }
    } else if field(head, "contained").is_some_and(|contained| contained.starts_with("yes")) {
        CrashKind::PartRestarted
    } else {
        CrashKind::Crashed
    };
    Some(CrashRecord {
        id: name.to_owned(),
        at,
        kind,
        reason,
        version: field(head, "version").map(str::to_owned),
        build: field(head, "build-id").map(str::to_owned),
    })
}

/// The start of `path`, at most `limit` bytes, as text; whether more
/// followed. Opened without following a link.
pub(crate) fn read_start(path: &Path, limit: u64) -> std::io::Result<(String, bool)> {
    let mut bytes = Vec::new();
    tilo_files::native::private::read(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let truncated = u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit;
    bytes.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

/// Every record in `dir`, newest first, without its text.
#[must_use]
pub fn list(dir: &Path) -> Vec<CrashRecord> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            (is_record_name(&name) && entry.file_type().ok()?.is_file()).then_some(name)
        })
        .collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    names
        .into_iter()
        .take(MAX_LISTED)
        .filter_map(|name| {
            let (head, _) = read_start(&dir.join(&name), HEAD_BYTES).ok()?;
            summarize(&name, &head)
        })
        .collect()
}

/// The record `id` in `dir`, with its text.
///
/// # Errors
///
/// No record is called `id`, or it could not be read.
pub fn read(dir: &Path, id: &str) -> Result<CrashRecordText, String> {
    if !is_record_name(id) {
        return Err(format!("no record is called {id}"));
    }
    let (text, truncated) =
        read_start(&dir.join(id), MAX_TEXT).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => format!("no record is called {id}"),
            _ => format!("the record {id} could not be read: {error}"),
        })?;
    let record = summarize(id, &text).ok_or_else(|| format!("no record is called {id}"))?;
    Ok(CrashRecordText {
        record,
        text,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_freeze;

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        tilo_files::native::private::directory(dir.path()).unwrap();
        dir
    }

    fn record(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        crate::write_record(dir, name, body).unwrap()
    }

    #[test]
    fn a_freeze_and_a_panic_are_told_apart_by_their_facts() {
        let build = crate::BuildInfo {
            version: "0.1.0",
            executable: None,
            build_id: Some("abc123".to_owned()),
        };
        let freeze = render_freeze(
            &build,
            &crate::Freeze {
                timestamp: "2026-10-24-225201",
                tid: 412,
                blocked_for: std::time::Duration::from_secs(26),
                report: "main loop hung",
            },
        );
        let name = "2026-10-24-225201-412-freeze.log";
        let summarized = summarize(name, &freeze).unwrap();
        assert_eq!(summarized.id, name);
        assert_eq!(summarized.kind, CrashKind::Froze);
        assert_eq!(summarized.version.as_deref(), Some("0.1.0"));
        assert_eq!(summarized.build.as_deref(), Some("abc123"));

        let panic = "tilo session panic\n==================\nversion:   0.1.0\ncontained: no: the session ends\n";
        assert_eq!(
            summarize("2026-10-24-210000-5.log", panic).unwrap().kind,
            CrashKind::Crashed
        );
        let contained = panic.replace("no: the session ends", "yes: the part restarts");
        assert_eq!(
            summarize("2026-10-24-210000-5.log", &contained)
                .unwrap()
                .kind,
            CrashKind::PartRestarted
        );
        assert_eq!(summarize("notes.log", panic), None);
        assert_eq!(summarize(".noticed", panic), None);
        assert_eq!(summarize("../2026-10-24-210000-5.log", panic), None);
    }

    #[test]
    fn the_computer_stopping_is_its_own_kind_and_a_fault_is_not_a_stop() {
        let head =
            "tilo computer stopped\n=====================\nversion:   0.1.0\nreason:    panic\n";
        let stopped = summarize("2026-10-24-225201-4f00-stopped.log", head).unwrap();
        assert_eq!(stopped.kind, CrashKind::ComputerStopped);
        assert_eq!(stopped.reason, Some(StopReason::Panic));
        let fault = head.replace("panic", "oops");
        let fault = summarize("2026-10-24-225201-4f00-stopped.log", &fault).unwrap();
        assert_eq!(fault.kind, CrashKind::SystemFault);
        assert_eq!(fault.reason, Some(StopReason::Oops));
    }

    #[test]
    fn a_placeholder_fact_is_no_fact() {
        let head = "tilo session panic\n==================\nversion:   0.1.0\nbuild-id:  <none>\n";
        let summarized = summarize("2026-10-24-210000-5.log", head).unwrap();
        assert_eq!(summarized.build, None);
        assert_eq!(summarized.version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn records_are_listed_newest_first_without_their_stacks() {
        let dir = private_dir();
        record(dir.path(), "2026-10-20-080000-7-freeze.log", "older\nstack");
        record(
            dir.path(),
            "2026-10-24-225201-9-freeze.log",
            &format!("newer\n{}", "f".repeat(8192)),
        );
        record(dir.path(), ".noticed", "2026-10-24-225201-9-freeze.log");
        let listed = list(dir.path());
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "2026-10-24-225201-9-freeze.log");
        assert_eq!(listed[1].id, "2026-10-20-080000-7-freeze.log");
    }

    #[test]
    fn one_record_is_read_by_the_name_the_list_gave() {
        let dir = private_dir();
        let path = record(dir.path(), "2026-10-24-225201-9-freeze.log", "the text");
        assert_eq!(path.file_name().unwrap(), "2026-10-24-225201-9-freeze.log");
        let record = read(dir.path(), "2026-10-24-225201-9-freeze.log").unwrap();
        assert_eq!(record.text, "the text");
        assert!(!record.truncated);
        assert_eq!(record.record.kind, CrashKind::Froze);
        assert!(read(dir.path(), "2026-10-25-000000-1-freeze.log").is_err());
        assert!(read(dir.path(), "../escape.log").is_err());
    }

    #[test]
    fn a_long_record_is_read_only_as_far_as_it_is_answered() {
        let dir = private_dir();
        let body = "x".repeat(usize::try_from(MAX_TEXT).unwrap() + 16);
        record(dir.path(), "2026-10-24-225201-9-freeze.log", &body);
        let record = read(dir.path(), "2026-10-24-225201-9-freeze.log").unwrap();
        assert!(record.truncated);
        assert_eq!(u64::try_from(record.text.len()).unwrap(), MAX_TEXT);
    }

    #[test]
    fn an_missing_directory_lists_nothing() {
        assert!(list(Path::new("/nonexistent/tilo/crashes")).is_empty());
    }
}
