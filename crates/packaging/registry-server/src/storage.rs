//! On-disk state for a registry — one JSON file per entry, plus a
//! separate `publishers.toml` for the owner-controlled
//! allowlist.
//!
//! Layout under the data dir (`--data-dir` or
//! `$XDG_STATE_HOME/qos-registry`):
//!
//! ```text
//! data-dir/
//!     handshake.toml       # server identity (name, description, base_url)
//!     publishers.toml      # owner-controlled allowlist
//!     entries/
//!         <urlencoded-pkg-id>.json   # one file per RegistryEntry
//!     pending/
//!         <urlencoded-pkg-id>.json   # one pending signed change per package
//!     reviews/
//!         <review-id>.json           # immutable moderator audit records
//!     quarantined/
//!         <urlencoded-pkg-id>.json   # entries removed from public visibility
//! ```
//!
//! Writes go through atomic rename (`.tmp` → real path) so a
//! crash mid-write cannot leave a torn file.
//!
//! The store is read-locked with `tokio::sync::RwLock` at the
//! server layer; this module deliberately owns no concurrency —
//! it's a thin path/serde wrapper.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use registry_types::{EntryAction, EntryChangeRequest, RegistryEntry};

use crate::allowlist::PublisherAllowlist;

/// Directory + on-disk file layout for one registry.
#[derive(Clone)]
pub struct Storage {
    root: PathBuf,
}

impl Storage {
    /// Open (creating on first use) the store rooted at `root`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the state directories can't be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root = root.into();
        for name in ["entries", "pending", "reviews", "quarantined"] {
            std::fs::create_dir_all(root.join(name))
                .map_err(|e| StorageError::Io(format!("mkdir {name}: {e}")))?;
        }
        Ok(Self { root })
    }

    /// The `handshake.toml` path.
    pub fn handshake_path(&self) -> PathBuf {
        self.root.join("handshake.toml")
    }

    /// The `publishers.toml` path.
    pub fn publishers_path(&self) -> PathBuf {
        self.root.join("publishers.toml")
    }

    /// Load the server-identity file. Missing → default-empty
    /// (used on first-run before `serve` writes one).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the file exists but can't be
    /// read, or [`StorageError::Parse`] if it isn't valid TOML.
    pub fn load_identity(&self) -> Result<ServerIdentity, StorageError> {
        let path = self.handshake_path();
        if !path.exists() {
            return Ok(ServerIdentity::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| StorageError::Io(format!("read handshake: {e}")))?;
        toml::from_str(&text).map_err(|e| StorageError::Parse(e.to_string()))
    }

    /// Atomic-rename write of the server-identity file.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Encode`] if `ident` fails to encode as
    /// TOML, or [`StorageError::Io`] if the write fails.
    pub fn save_identity(&self, ident: &ServerIdentity) -> Result<(), StorageError> {
        let path = self.handshake_path();
        let body =
            toml::to_string_pretty(ident).map_err(|e| StorageError::Encode(e.to_string()))?;
        atomic_write(&path, body.as_bytes())
    }

    /// Load the publisher allowlist. Missing → empty (nobody can
    /// post).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the file exists but can't be
    /// read, or [`StorageError::Parse`] if it isn't valid TOML.
    pub fn load_allowlist(&self) -> Result<PublisherAllowlist, StorageError> {
        let path = self.publishers_path();
        if !path.exists() {
            return Ok(PublisherAllowlist::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| StorageError::Io(format!("read publishers: {e}")))?;
        PublisherAllowlist::from_toml(&text).map_err(|e| StorageError::Parse(e.to_string()))
    }

    /// Atomic-rename write of the allowlist.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Encode`] if `allow` fails to encode as
    /// TOML, or [`StorageError::Io`] if the write fails.
    pub fn save_allowlist(&self, allow: &PublisherAllowlist) -> Result<(), StorageError> {
        let path = self.publishers_path();
        let body = allow
            .to_toml()
            .map_err(|e| StorageError::Encode(e.to_string()))?;
        atomic_write(&path, body.as_bytes())
    }

    /// One entry's file path — a filesystem-safe rendering of the
    /// package id.
    fn entry_path(&self, package_id: &str) -> PathBuf {
        self.root
            .join("entries")
            .join(format!("{}.json", filename_encode(package_id)))
    }

    /// One quarantined entry's file path.
    fn quarantined_entry_path(&self, package_id: &str) -> PathBuf {
        self.root
            .join("quarantined")
            .join(format!("{}.json", filename_encode(package_id)))
    }

    /// One pending submission's file path.
    fn pending_path(&self, submission_id: &str) -> PathBuf {
        self.root
            .join("pending")
            .join(format!("{}.json", filename_encode(submission_id)))
    }

    /// One review record's file path.
    fn review_path(&self, review_id: &str) -> PathBuf {
        self.root
            .join("reviews")
            .join(format!("{}.json", filename_encode(review_id)))
    }

    /// Process-wide lock used by the server and local reviewer CLI to
    /// serialize multi-file moderation transitions.
    pub(crate) fn moderation_lock_path(&self) -> PathBuf {
        self.root.join("moderation.lock")
    }

    /// Load every entry from disk. Order is not defined.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the `entries/` directory or any
    /// `.json` file in it can't be read, or [`StorageError::Parse`] if
    /// one fails to decode as a [`RegistryEntry`].
    pub fn load_entries(&self) -> Result<Vec<RegistryEntry>, StorageError> {
        let dir = self.root.join("entries");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|e| StorageError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| StorageError::Io(e.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| StorageError::Io(format!("read entry {}: {e}", path.display())))?;
            let parsed: RegistryEntry =
                serde_json::from_str(&text).map_err(|e| StorageError::Parse(e.to_string()))?;
            out.push(parsed);
        }
        Ok(out)
    }

    /// Load one entry by id, or `Ok(None)` if it isn't on disk.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the file exists but can't be
    /// read, or [`StorageError::Parse`] if it fails to decode as a
    /// [`RegistryEntry`].
    pub fn load_entry(&self, package_id: &str) -> Result<Option<RegistryEntry>, StorageError> {
        let path = self.entry_path(package_id);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| StorageError::Io(format!("read entry: {e}")))?;
        let parsed: RegistryEntry =
            serde_json::from_str(&text).map_err(|e| StorageError::Parse(e.to_string()))?;
        Ok(Some(parsed))
    }

    /// Atomic-rename write of one entry.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Encode`] if `entry` fails to encode as
    /// JSON, or [`StorageError::Io`] if the write fails.
    pub fn save_entry(&self, entry: &RegistryEntry) -> Result<(), StorageError> {
        let path = self.entry_path(&entry.package_id);
        let body =
            serde_json::to_vec_pretty(entry).map_err(|e| StorageError::Encode(e.to_string()))?;
        atomic_write(&path, &body)
    }

    /// Remove an entry from disk. `Ok(false)` if it wasn't there.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the file exists but can't be removed.
    pub fn delete_entry(&self, package_id: &str) -> Result<bool, StorageError> {
        delete_if_exists(&self.entry_path(package_id))
    }

    /// Load an entry that moderation has removed from public visibility.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry cannot be read or decoded.
    pub fn load_quarantined_entry(
        &self,
        package_id: &str,
    ) -> Result<Option<RegistryEntry>, StorageError> {
        load_json(
            &self.quarantined_entry_path(package_id),
            "quarantined entry",
        )
    }

    /// Atomically persist an entry outside the public index.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry cannot be encoded or persisted.
    pub fn save_quarantined_entry(&self, entry: &RegistryEntry) -> Result<(), StorageError> {
        save_json(&self.quarantined_entry_path(&entry.package_id), entry)
    }

    /// Remove an entry from quarantine after it has been restored.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry exists but cannot be removed.
    pub fn delete_quarantined_entry(&self, package_id: &str) -> Result<bool, StorageError> {
        delete_if_exists(&self.quarantined_entry_path(package_id))
    }

    /// Load one pending publisher submission.
    ///
    /// # Errors
    ///
    /// Returns an error when the submission cannot be read or decoded.
    pub fn load_pending(
        &self,
        submission_id: &str,
    ) -> Result<Option<PendingSubmission>, StorageError> {
        load_json(&self.pending_path(submission_id), "pending submission")
    }

    /// Load all pending submissions. Ordering is not defined.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue cannot be read or decoded.
    pub fn load_pending_submissions(&self) -> Result<Vec<PendingSubmission>, StorageError> {
        load_json_dir(&self.root.join("pending"), "pending submission")
    }

    /// Atomically create or replace a pending submission.
    ///
    /// # Errors
    ///
    /// Returns an error when the submission cannot be encoded or persisted.
    pub fn save_pending(&self, submission: &PendingSubmission) -> Result<(), StorageError> {
        save_json(&self.pending_path(&submission.submission_id), submission)
    }

    /// Remove a pending submission after its terminal review record is durable.
    ///
    /// # Errors
    ///
    /// Returns an error when the submission exists but cannot be removed.
    pub fn delete_pending(&self, submission_id: &str) -> Result<bool, StorageError> {
        delete_if_exists(&self.pending_path(submission_id))
    }

    /// Load one durable moderator review/audit record.
    ///
    /// # Errors
    ///
    /// Returns an error when the review cannot be read or decoded.
    pub fn load_review(&self, review_id: &str) -> Result<Option<ReviewRecord>, StorageError> {
        load_json(&self.review_path(review_id), "review")
    }

    /// Load all moderator review/audit records. Ordering is not defined.
    ///
    /// # Errors
    ///
    /// Returns an error when the review log cannot be read or decoded.
    pub fn load_reviews(&self) -> Result<Vec<ReviewRecord>, StorageError> {
        load_json_dir(&self.root.join("reviews"), "review")
    }

    /// Atomically persist a review/audit record.
    ///
    /// # Errors
    ///
    /// Returns an error when the review cannot be encoded or persisted.
    pub fn save_review(&self, review: &ReviewRecord) -> Result<(), StorageError> {
        save_json(&self.review_path(&review.review_id), review)
    }
}

fn load_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    label: &str,
) -> Result<Option<T>, StorageError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| StorageError::Io(format!("read {label} {}: {e}", path.display())))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| StorageError::Parse(e.to_string()))
}

fn load_json_dir<T: serde::de::DeserializeOwned>(
    dir: &Path,
    label: &str,
) -> Result<Vec<T>, StorageError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| StorageError::Io(e.to_string()))? {
        let path = entry.map_err(|e| StorageError::Io(e.to_string()))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(value) = load_json(&path, label)? {
            out.push(value);
        }
    }
    Ok(out)
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StorageError> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| StorageError::Encode(e.to_string()))?;
    atomic_write(path, &body)
}

fn delete_if_exists(path: &Path) -> Result<bool, StorageError> {
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(path).map_err(|e| StorageError::Io(e.to_string()))?;
    // Match `atomic-file`'s durability property for the transition's delete
    // side: a successful return should not resurrect this state after power
    // loss because the containing directory entry was never flushed.
    if let Some(parent) = path.parent()
        && let Ok(directory) = std::fs::File::open(parent)
    {
        let _result = directory.sync_all();
    }
    Ok(true)
}

/// A publisher request awaiting a moderator's decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSubmission {
    /// The package id. There is at most one pending revision for a package,
    /// so a newer revision can atomically replace its predecessor.
    pub submission_id: String,
    /// Time the registry accepted this pending revision.
    pub submitted_at_unix: u64,
    /// The publisher-signed request that will be re-verified at approval.
    pub change: EntryChangeRequest,
}

/// The terminal effect recorded for a moderator action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    Rejected,
    Quarantined,
    Restored,
}

/// What an audit record reviewed. Publisher submissions retain the complete
/// signed request; operational quarantine/restore records retain the entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewSubject {
    Submission(PendingSubmission),
    Entry {
        action: EntryAction,
        entry: RegistryEntry,
    },
}

/// Durable audit record for every terminal moderation decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRecord {
    /// Globally unique ID for this decision.
    pub review_id: String,
    /// Result of the moderation action.
    pub decision: ReviewDecision,
    /// Moderator identity, supplied by the caller.
    pub reviewer: String,
    /// Operator-supplied explanation; an empty string is allowed.
    pub reason: String,
    /// Time this terminal decision was made.
    pub reviewed_at_unix: u64,
    /// The exact data reviewed.
    pub subject: ReviewSubject,
}

/// The server's own greeting: name, description, `base_url` plus
/// the owner pubkey pinned by clients. Sits in `handshake.toml`
/// so operators can edit it with a text editor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerIdentity {
    /// Owner Ed25519 pubkey (32 bytes, hex).
    pub owner_pubkey_hex: String,
    /// Human-readable registry name.
    pub name: String,
    /// Human-readable registry description.
    pub description: String,
    /// Public base URL the registry advertises for itself.
    pub base_url: String,
}

/// Write bytes to `path` via a `.tmp` sibling + rename. Both the
/// tmp and the final file end up in the same directory so the
/// rename is atomic on every POSIX filesystem qOS supports.
///
/// # Errors
///
/// Returns [`StorageError::Io`] if `path`'s parent can't be created
/// or the write/rename fails.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| StorageError::Io(e.to_string()))?;
    }
    atomic_file::write(path, bytes).map_err(|e| StorageError::Io(e.to_string()))?;
    Ok(())
}

/// Percent-encode-ish rendering of a package id so it round-trips
/// through the filesystem on macOS + Linux + a case-preserving
/// fallback. Reverse-DNS ids ("dev.qos.editor") already only
/// contain safe characters, but we lower-case them and swap `/`
/// for `_` defensively against future publishers.
fn filename_encode(pkg_id: &str) -> String {
    let mut out = String::with_capacity(pkg_id.len());
    for ch in pkg_id.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => out.push(ch),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "_{:02x}", ch as u32);
            }
        }
    }
    out
}

/// Errors surfaced by disk I/O + serde encode/decode.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("I/O: {0}")]
    Io(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("encode: {0}")]
    Encode(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(id: &str) -> RegistryEntry {
        RegistryEntry {
            package_id: id.into(),
            name: "Test".into(),
            description: String::new(),
            categories: vec![],
            icon_url: None,
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: "aa".repeat(32),
            versions: vec![],
            updated_at_unix: 100,
            signature_hex: String::new(),
        }
    }

    #[test]
    fn open_creates_layout() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        assert!(s.root.join("entries").is_dir());
        assert!(s.root.join("pending").is_dir());
        assert!(s.root.join("reviews").is_dir());
        assert!(s.root.join("quarantined").is_dir());
    }

    #[test]
    fn save_and_load_entry_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        let e = sample_entry("dev.qos.editor");
        s.save_entry(&e).unwrap();
        let back = s.load_entry("dev.qos.editor").unwrap().unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn load_entry_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        assert!(s.load_entry("nope").unwrap().is_none());
    }

    #[test]
    fn load_entries_lists_all() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        s.save_entry(&sample_entry("dev.qos.a")).unwrap();
        s.save_entry(&sample_entry("dev.qos.b")).unwrap();
        let mut ids: Vec<String> = s
            .load_entries()
            .unwrap()
            .into_iter()
            .map(|e| e.package_id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["dev.qos.a", "dev.qos.b"]);
    }

    #[test]
    fn delete_entry_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        s.save_entry(&sample_entry("dev.qos.a")).unwrap();
        assert!(s.delete_entry("dev.qos.a").unwrap());
        assert!(!s.delete_entry("dev.qos.a").unwrap());
        assert!(s.load_entry("dev.qos.a").unwrap().is_none());
    }

    #[test]
    fn filename_encode_escapes_unsafe_chars() {
        assert_eq!(filename_encode("dev.qos.editor"), "dev.qos.editor");
        // A slash gets escaped so it can't escape the entries/
        // directory.
        assert_eq!(filename_encode("evil/../etc"), "evil_2f.._2fetc");
    }

    #[test]
    fn identity_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        let ident = ServerIdentity {
            owner_pubkey_hex: "cc".repeat(32),
            name: "qOS Official".into(),
            description: "The default registry".into(),
            base_url: "https://apps.qos.dev".into(),
        };
        s.save_identity(&ident).unwrap();
        let back = s.load_identity().unwrap();
        assert_eq!(back, ident);
    }
}
