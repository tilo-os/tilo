//! Who changed which setting: a bounded log beside the session's records.
//!
//! Every leaf a session write changes is appended as one JSON line to
//! `records/changes.jsonl`, with the value before and after and the
//! [`ChangeOrigin`] the writer gave. A write another process made to the
//! copy has no origin (`"via": "copy"`). The log is the answer to "who
//! changed this" and the input a system undo needs; it is not the stored
//! truth, so a log that cannot be written never refuses a change.
//!
//! The log is bounded: once it outgrows [`CAPACITY`] bytes it is rewritten
//! with its newest half.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::ChangeOrigin;

/// The size past which the log drops its oldest half.
pub(super) const CAPACITY: u64 = 256 * 1024;

/// The log's file name in the records directory.
const FILE: &str = "changes.jsonl";

/// One leaf a write changed.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LeafChange {
    pub(super) section: String,
    /// `None` for a top-level value that is not a section (`version`).
    pub(super) leaf: Option<String>,
    pub(super) before: Option<Value>,
    pub(super) after: Value,
}

/// Where the log of the records at `person` lives: beside them.
pub(super) fn path_beside(person: &Path) -> PathBuf {
    person.with_file_name(FILE)
}

/// Append `changes`, made by `origin` (`None`: taken in from the copy).
/// Best effort: the change is already durable in the records.
pub(super) fn append(path: &Path, changes: &[LeafChange], origin: Option<&ChangeOrigin>) {
    if changes.is_empty() {
        return;
    }
    let at_ms = tilo_clock::unix_now_ms();
    let via = if origin.is_some() { "session" } else { "copy" };
    let origin = origin.cloned().unwrap_or_default();
    let mut lines = String::new();
    for change in changes {
        let line = json!({
            "at_ms": at_ms,
            "section": change.section,
            "leaf": change.leaf,
            "before": change.before,
            "after": change.after,
            "action": origin.action,
            "principal": origin.principal,
            "via": via,
        });
        lines.push_str(&line.to_string());
        lines.push('\n');
    }
    let Ok((dir, name)) = tilo_files::file_at(path) else {
        return;
    };
    if tilo_files::blocking::append(&dir, &name, lines.as_bytes()).is_err() {
        return;
    }
    let too_long =
        tilo_files::blocking::metadata(&dir, &name).is_ok_and(|meta| meta.len > CAPACITY);
    if too_long {
        compact(path);
    }
}

/// Rewrite the log with the newest lines that fit in half its capacity.
fn compact(path: &Path) {
    let Some(text) = read_text(path) else {
        return;
    };
    let budget = usize::try_from(CAPACITY.div_euclid(2)).unwrap_or(usize::MAX);
    let mut kept = Vec::new();
    let mut size = 0_usize;
    for line in text.lines().rev() {
        size = size.saturating_add(line.len().saturating_add(1));
        if size > budget {
            break;
        }
        kept.push(line);
    }
    kept.reverse();
    let mut body = kept.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    let _ = tilo_files::native::atomic::write_creating_parent(path, body.as_bytes());
}

/// Every entry in the log at `path`, oldest first. Lines that do not parse
/// (a crash mid-append) are skipped.
#[must_use]
pub(super) fn read(path: &Path) -> Vec<Value> {
    read_text(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The log's text, or `None` when it cannot be read.
fn read_text(path: &Path) -> Option<String> {
    let (dir, name) = tilo_files::file_at(path).ok()?;
    String::from_utf8(tilo_files::blocking::read(&dir, &name).ok()?).ok()
}
