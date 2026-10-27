//! The broker's audit log.
//!
//! Append-only JSON Lines under `audit/`, one file per day. Every request a
//! guest makes of the broker is recorded with its outcome, allowed or
//! refused — a log that only records grants can't answer "what did that
//! shell try to do".
//!
//! A *file*, not the compositor's in-memory ring
//! (`crates/session/shell/src/shell/collab_broker.rs`): that ring records collaboration
//! ops keyed by `ArtifactId`, not a `trust-dir` request, and doesn't
//! survive a restart — the wrong property for what you consult after the
//! fact. What broker decisions share with it is only the vocabulary:
//! `decide_capability` on a `CapOverride` (see [`crate::policy`]).
//!
//! The log lives in the store, never bound into a guest, so nothing inside
//! can read or rewrite its own history.

use std::io::Write;
use std::path::{Path, PathBuf};
use tilo_clock::unix_now;

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuditRecord {
    /// Seconds since the Unix epoch.
    pub at: u64,
    pub session: String,
    pub environment: String,
    /// What was asked: `trust-dir`, `pkg-install`, `open-session`, …
    pub request: String,
    /// What it was asked about: a path, a package name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub allowed: bool,
    /// Why, when it matters — the refusal reason, or the capability id minted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl AuditRecord {
    pub fn new(session: &str, environment: &str, request: &str, allowed: bool) -> Self {
        Self {
            at: unix_now(),
            session: session.to_string(),
            environment: environment.to_string(),
            request: request.to_string(),
            subject: None,
            allowed,
            detail: None,
        }
    }

    #[must_use]
    pub fn about(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    #[must_use]
    pub fn because(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// The append-only log in a directory.
#[derive(Debug, Clone)]
pub struct AuditLog {
    dir: PathBuf,
}

impl AuditLog {
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file a record at `at` lands in: `audit/<day>.jsonl`, `day` counting
    /// whole days since epoch — one file per day, so "yesterday" is a `cat`.
    pub fn file_for(&self, at: u64) -> PathBuf {
        self.dir.join(format!("{}.jsonl", at / 86_400))
    }

    /// Append a record. A failure here is logged and swallowed: the audit log
    /// records what the broker did, not a precondition for doing it — refusing
    /// a legitimate request over a full disk would turn that into a denial of
    /// service.
    pub fn append(&self, record: &AuditRecord) {
        if let Err(e) = self.try_append(record) {
            tracing::warn!(error = %e, "could not write the audit record; the decision still stands");
        }
    }

    fn try_append(&self, record: &AuditRecord) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.file_for(record.at))?;
        file.write_all(&line)?;
        file.flush()
    }

    /// Read the whole log, oldest first. Malformed lines are skipped rather
    /// than aborting the read — one bad line must not hide the rest.
    pub fn read_all(&self) -> Vec<AuditRecord> {
        let mut files: Vec<PathBuf> = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect(),
            Err(_) => return Vec::new(),
        };
        files.sort();
        let mut out = Vec::new();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            for line in text.lines() {
                if let Ok(rec) = serde_json::from_str::<AuditRecord>(line) {
                    out.push(rec);
                }
            }
        }
        out.sort_by_key(|r| r.at);
        out
    }

    /// The most recent `n` records, newest last.
    pub fn tail(&self, n: usize) -> Vec<AuditRecord> {
        let all = self.read_all();
        let skip = all.len().saturating_sub(n);
        all.into_iter().skip(skip).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_keeps_every_grant_and_refusal_it_is_given() {
        // phase: records_round_trip_through_the_log
        {
            let dir = tempfile::tempdir().unwrap();
            let log = AuditLog::at(dir.path().join("audit"));

            log.append(
                &AuditRecord::new("s1", "default", "trust-dir", true).about("/var/home/you/src"),
            );
            log.append(
                &AuditRecord::new("s1", "default", "trust-dir", false)
                    .about("/usr")
                    .because("the overlay owns this subtree"),
            );

            let all = log.read_all();
            assert_eq!(all.len(), 2);
            assert!(all[0].allowed);
            assert_eq!(all[0].subject.as_deref(), Some("/var/home/you/src"));
            assert!(!all[1].allowed);
            assert_eq!(
                all[1].detail.as_deref(),
                Some("the overlay owns this subtree")
            );
        }
        // phase: a_refusal_is_recorded_as_loudly_as_a_grant
        // A log that only recorded successes could not answer the question it
        // exists for.
        {
            let dir = tempfile::tempdir().unwrap();
            let log = AuditLog::at(dir.path().join("audit"));
            log.append(&AuditRecord::new("s1", "default", "pkg-install", false).about("nethack"));
            let all = log.read_all();
            assert_eq!(all.len(), 1);
            assert!(!all[0].allowed);
            assert_eq!(all[0].request, "pkg-install");
        }
        // phase: the_log_appends_rather_than_replaces
        {
            let dir = tempfile::tempdir().unwrap();
            let log = AuditLog::at(dir.path().join("audit"));
            for i in 0..5 {
                log.append(
                    &AuditRecord::new("s1", "default", "pkg-install", true).about(format!("p{i}")),
                );
            }
            assert_eq!(log.read_all().len(), 5);
            // A second handle to the same directory sees everything the first wrote.
            let reopened = AuditLog::at(dir.path().join("audit"));
            assert_eq!(reopened.read_all().len(), 5);
        }
        // phase: a_malformed_line_does_not_hide_the_rest
        {
            let dir = tempfile::tempdir().unwrap();
            let audit = dir.path().join("audit");
            let log = AuditLog::at(&audit);
            log.append(&AuditRecord::new("s1", "default", "trust-dir", true));
            std::fs::OpenOptions::new()
                .append(true)
                .open(log.file_for(unix_now()))
                .unwrap()
                .write_all(b"{ truncated\n")
                .unwrap();
            log.append(&AuditRecord::new("s1", "default", "pkg-install", true));
            assert_eq!(log.read_all().len(), 2);
        }
    }

    #[test]
    fn the_log_tails_the_newest_records_and_rotates_by_day() {
        // phase: tail_returns_the_newest_records
        {
            let dir = tempfile::tempdir().unwrap();
            let log = AuditLog::at(dir.path().join("audit"));
            for i in 0..10 {
                let mut r =
                    AuditRecord::new("s1", "default", "pkg-install", true).about(format!("p{i}"));
                r.at = 1_000_000 + i;
                log.append(&r);
            }
            let tail = log.tail(3);
            assert_eq!(tail.len(), 3);
            assert_eq!(tail[2].subject.as_deref(), Some("p9"));
        }
        // phase: the_log_rotates_by_day
        {
            let dir = tempfile::tempdir().unwrap();
            let log = AuditLog::at(dir.path().join("audit"));
            assert_ne!(log.file_for(0), log.file_for(86_400));
            assert_eq!(log.file_for(0), log.file_for(86_399));
        }
    }
}
