//! The one-time import of the history database earlier releases kept beside
//! the namespace document.
//!
//! Immutable history is part of the tree document now, so a workspace has one
//! source for it. A device upgrading from the separate `file-history.json`
//! and its append-only log keeps everything it recorded: every version,
//! conflict and resolution is written into the document with the author and
//! time it already carried, and only then are the old files moved aside.
//!
//! Running it again is harmless. A version the document holds is a replay, so
//! a crash between importing and moving the files aside loses nothing and
//! duplicates nothing.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use fs_protocol::{ConflictResolution, FileConflictV1, FileRecordId, FileVersionId, FileVersionV1};
use serde::Deserialize;
use uuid::Uuid;

use crate::Namespace;

/// The document format the separate store wrote.
const SCHEMA_VERSION: u16 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryDocument {
    schema_version: u16,
    records: Vec<RecordDocument>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordDocument {
    /// Part of the stored format; each version names its own record.
    #[expect(
        dead_code,
        reason = "the document's key restates what its versions carry"
    )]
    record_id: FileRecordId,
    versions: Vec<FileVersionV1>,
    /// The old store's cached heads. The document derives them from the
    /// versions' parents, so they are read and discarded.
    #[expect(dead_code, reason = "heads are derived from the versions")]
    heads: Vec<FileVersionId>,
    conflicts: Vec<FileConflictV1>,
}

/// One durable mutation the old store appended after its snapshot.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum LogEntry {
    Versions {
        versions: Vec<FileVersionV1>,
    },
    Conflict {
        conflict: FileConflictV1,
    },
    Resolve {
        record_id: FileRecordId,
        conflict_id: Uuid,
        resolution: ConflictResolution,
    },
}

/// Import `path` and its log into `namespace`, returning whether the document
/// gained anything.
///
/// # Errors
///
/// Returns an error, leaving both files in place for a later run, when the
/// database is from another release, cannot be read, or does not replay
/// completely.
pub fn import(namespace: &mut Namespace, path: &Path) -> Result<bool> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let document: HistoryDocument =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    if document.schema_version != SCHEMA_VERSION {
        bail!(
            "{} holds history schema version {}",
            path.display(),
            document.schema_version
        );
    }
    let mut imported = 0_usize;
    for record in document.records {
        imported = imported.saturating_add(append(namespace, &record.versions)?);
        for conflict in record.conflicts {
            if namespace.record_history_conflict(&conflict)? {
                imported = imported.saturating_add(1);
            }
        }
    }
    let log = log_path(path);
    match std::fs::read(&log) {
        Ok(bytes) => imported = imported.saturating_add(replay(namespace, &log, &bytes)?),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("read {}", log.display())),
    }
    Ok(imported > 0)
}

/// Move `path` and its log aside, once what they hold is in the document.
pub fn retire(path: &Path) {
    for file in [path.to_path_buf(), log_path(path)] {
        let mut aside = file.as_os_str().to_owned();
        aside.push(".imported");
        if let Err(error) = std::fs::rename(&file, PathBuf::from(aside))
            && error.kind() != ErrorKind::NotFound
        {
            eprintln!("could not move {} aside: {error}", file.display());
        }
    }
}

/// Write `versions` into the document, returning how many were new to it.
fn append(namespace: &mut Namespace, versions: &[FileVersionV1]) -> Result<usize> {
    Ok(namespace
        .append_versions(versions)?
        .into_iter()
        .filter(|appended| *appended)
        .count())
}

/// Replay the log's good prefix. A torn final line is a write that never
/// returned; anything before it was acknowledged and must replay.
fn replay(namespace: &mut Namespace, path: &Path, bytes: &[u8]) -> Result<usize> {
    let mut imported = 0_usize;
    let mut rest = bytes;
    while let Some(newline) = rest.iter().position(|byte| *byte == b'\n') {
        let (line, tail) = rest.split_at(newline);
        let tail = tail.get(1..).unwrap_or_default();
        let Some(entry) = decode(line) else {
            if tail.is_empty() {
                break;
            }
            bail!(
                "{} is damaged at byte {}",
                path.display(),
                bytes.len().saturating_sub(rest.len())
            );
        };
        imported = imported.saturating_add(apply(namespace, entry)?);
        rest = tail;
    }
    Ok(imported)
}

/// Apply one logged mutation, returning how many history entries it added.
fn apply(namespace: &mut Namespace, entry: LogEntry) -> Result<usize> {
    match entry {
        LogEntry::Versions { versions } => append(namespace, &versions),
        LogEntry::Conflict { conflict } => {
            Ok(usize::from(namespace.record_history_conflict(&conflict)?))
        }
        LogEntry::Resolve {
            record_id,
            conflict_id,
            resolution,
        } => Ok(usize::from(namespace.resolve_history_conflict(
            record_id,
            conflict_id,
            &resolution,
        )?)),
    }
}

/// One log line: the blake3 digest of the entry, a space, the entry.
fn decode(line: &[u8]) -> Option<LogEntry> {
    let (digest, json) = line.split_first_chunk::<64>()?;
    let json = json.strip_prefix(b" ")?;
    if blake3::hash(json).to_hex().as_bytes() != digest {
        return None;
    }
    serde_json::from_slice(json).ok()
}

fn log_path(path: &Path) -> PathBuf {
    let mut log = path.as_os_str().to_owned();
    log.push(".log");
    PathBuf::from(log)
}

#[cfg(test)]
mod tests {
    use fs_protocol::{ContentHash, Ed25519PublicKey, FileActor, FileProvenance, FileVersionId};

    use super::*;

    fn actor() -> FileActor {
        FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        }
    }

    fn version(
        record_id: FileRecordId,
        parent: Option<FileVersionId>,
        timestamp_ms: u64,
    ) -> FileVersionV1 {
        FileVersionV1 {
            id: FileVersionId::new(),
            record_id,
            parents: parent.into_iter().collect(),
            actor: actor(),
            timestamp_ms,
            content_hash: Some(ContentHash([4; 32])),
            metadata_hash: ContentHash([5; 32]),
            provenance: FileProvenance::Edit,
        }
    }

    /// What an earlier release wrote: a snapshot of two versions, and a log
    /// holding one more version and a conflict resolution.
    #[test]
    fn imports_stored_versions_conflicts_and_log_entries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file-history.json");
        let record = FileRecordId::new();
        let first = version(record, None, 10);
        let second = version(record, Some(first.id), 20);
        let third = version(record, Some(second.id), 30);
        let conflict = FileConflictV1 {
            id: Uuid::new_v4(),
            record_id: record,
            versions: vec![first.id, second.id],
            status: fs_protocol::ConflictStatus::Open,
            resolution: None,
        };
        let document = serde_json::json!({
            "schema_version": 1,
            "records": [{
                "record_id": record,
                "heads": [second.id],
                "versions": [first, second],
                "conflicts": [conflict],
            }],
        });
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let logged = serde_json::json!({ "op": "versions", "versions": [third] });
        let json = serde_json::to_vec(&logged).unwrap();
        let mut line = blake3::hash(&json).to_hex().as_bytes().to_vec();
        line.push(b' ');
        line.extend_from_slice(&json);
        line.push(b'\n');
        std::fs::write(log_path(&path), &line).unwrap();

        let mut namespace = Namespace::new("importer").unwrap();
        assert!(import(&mut namespace, &path).unwrap());
        let history = namespace.record_history(record).unwrap();
        assert_eq!(history.versions, vec![first, second, third.clone()]);
        assert_eq!(history.heads, vec![third.id]);
        assert_eq!(history.conflicts, vec![conflict]);

        // A second run recognises every entry as present and imports nothing.
        assert!(!import(&mut namespace, &path).unwrap());
        assert_eq!(namespace.record_history(record).unwrap(), history);

        retire(&path);
        assert!(!path.exists());
        assert!(!log_path(&path).exists());
        // Nothing to retire twice.
        retire(&path);
    }

    /// A database this release cannot read is left where it is.
    #[test]
    fn leaves_an_unreadable_database_in_place() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file-history.json");
        std::fs::write(&path, br#"{"schema_version": 2, "records": []}"#).unwrap();
        let mut namespace = Namespace::new("importer").unwrap();
        assert!(import(&mut namespace, &path).is_err());
        assert!(path.exists());
    }
}
