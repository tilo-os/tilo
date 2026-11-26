//! The broker's audit log: append-only JSON Lines under `audit/`, one file per day.
//!
//! Every request a guest makes of the broker is recorded with its outcome, allowed or
//! refused — a log of grants alone cannot answer what a shell tried to do. The log
//! lives in the store, never bound into a guest, so nothing inside can read or rewrite
//! its own history.

use std::io::Write;
use std::path::{Path, PathBuf};
use tilo_clock::unix_now;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuditEntry {
    pub at: u64,
    pub session: String,
    pub environment: String,
    pub request: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub allowed: bool,
    /// Why, when it matters — the refusal reason, or the capability id minted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl AuditEntry {
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
    pub fn append(&self, record: &AuditEntry) {
        if let Err(e) = self.try_append(record) {
            tracing::warn!(error = %e, "could not write the audit record; the decision still stands");
        }
    }

    fn try_append(&self, record: &AuditEntry) -> std::io::Result<()> {
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
    pub fn read_all(&self) -> Vec<AuditEntry> {
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
                if let Ok(rec) = serde_json::from_str::<AuditEntry>(line) {
                    out.push(rec);
                }
            }
        }
        out.sort_by_key(|r| r.at);
        out
    }

    pub fn tail(&self, n: usize) -> Vec<AuditEntry> {
        let all = self.read_all();
        let skip = all.len().saturating_sub(n);
        all.into_iter().skip(skip).collect()
    }
}
