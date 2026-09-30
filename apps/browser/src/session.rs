//! Versioned normal-session recovery. Private windows never open this store.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    Normal,
    Private,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedTab {
    pub url: String,
    pub pinned: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub tabs: Vec<SavedTab>,
    pub active: usize,
    pub clean_exit: bool,
}
pub struct SessionStore {
    root: PathBuf,
    _lock: advisory_lock::FileLock,
}
impl Snapshot {
    /// Validate session version, URLs, active selection and pinned ordering.
    ///
    /// # Errors
    /// Returns an error for unsupported versions, unsafe URLs or inconsistent tab state.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.tabs.is_empty()
            || self.tabs.len() > 32
            || self.active >= self.tabs.len()
            || self.tabs.iter().any(|tab| !safe_url(&tab.url))
        {
            return Err("Invalid or unsupported saved browser session".into());
        }
        let mut unpinned = false;
        for tab in &self.tabs {
            if !tab.pinned {
                unpinned = true;
            } else if unpinned {
                return Err("Pinned tabs must precede unpinned tabs".into());
            }
        }
        Ok(())
    }
}
impl SessionStore {
    /// Lock the normal session store; private mode returns no store.
    ///
    /// # Errors
    /// Returns an error for a relative root, an existing process lock, or filesystem failures.
    pub fn open(root: &Path, mode: SessionMode) -> Result<Option<Self>, String> {
        if mode == SessionMode::Private {
            return Ok(None);
        }
        if !root.is_absolute() {
            return Err("Browser app root must be absolute".into());
        }
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let lock = advisory_lock::FileLock::try_open(
            &root.join("session.lock"),
            advisory_lock::LockMode::Exclusive,
        )
        .map_err(|error| match error {
            advisory_lock::TryLockError::WouldBlock => {
                "This browser app root is already open in another process".to_string()
            }
            advisory_lock::TryLockError::Error(error) => error.to_string(),
        })?;
        // An interrupted replacement can leave a temp file. Never follow it.
        let next = root.join("session.next");
        if std::fs::symlink_metadata(&next)
            .is_ok_and(|m| m.file_type().is_file() || m.file_type().is_symlink())
        {
            std::fs::remove_file(next).map_err(|e| e.to_string())?;
        }
        Ok(Some(Self {
            root: root.into(),
            _lock: lock,
        }))
    }
    /// Read and validate a saved session if one exists.
    ///
    /// # Errors
    /// Returns an error for unreadable, oversized, malformed or invalid session data.
    pub fn load(&self) -> Result<Option<Snapshot>, String> {
        use std::io::Read;
        let file = match std::fs::File::open(self.root.join("session.json")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let mut bytes = Vec::new();
        file.take(300_001)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 300_000 {
            return Err("Saved browser session exceeds size limit".into());
        }
        let snapshot: Snapshot = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        snapshot.validate()?;
        Ok(Some(snapshot))
    }
    /// Atomically replace the saved session after validation.
    ///
    /// # Errors
    /// Returns an error for invalid session state, serialization or filesystem failures.
    pub fn save(&self, snapshot: &Snapshot) -> Result<(), String> {
        snapshot.validate()?;
        let bytes = serde_json::to_vec(snapshot).map_err(|e| e.to_string())?;
        if bytes.len() > 300_000 {
            return Err("Saved browser session exceeds size limit".into());
        }
        tilo_files::native::atomic::write_with_mode(&self.root.join("session.json"), &bytes, 0o600)
            .map_err(|e| e.to_string())
    }
}
pub fn safe_url(value: &str) -> bool {
    if value == "about:blank" {
        return true;
    }
    if value.len() > 8192 || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    url::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Snapshot {
        Snapshot {
            version: 1,
            tabs: vec![
                SavedTab {
                    url: "https://alpha.test/".into(),
                    pinned: true,
                },
                SavedTab {
                    url: "https://beta.test/committed".into(),
                    pinned: false,
                },
            ],
            active: 1,
            clean_exit: false,
        }
    }
    /// Disk round trip, budgets, malformed input and write policy.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn session_snapshots_persist_atomically_and_refuse_unsafe_writes() {
        // Phase `disk_roundtrip_and_atomic_replacement_preserve_last_valid_on_failure`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = SessionStore::open(root.path(), SessionMode::Normal)
                .unwrap()
                .unwrap();
            store.save(&fixture()).unwrap();
            assert_eq!(store.load().unwrap(), Some(fixture()));
            let mut invalid = fixture();
            invalid.active = 9;
            assert!(store.save(&invalid).is_err());
            assert_eq!(store.load().unwrap(), Some(fixture()));
        }

        // Phase `serialized_budget_failure_preserves_previous_snapshot`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = SessionStore::open(root.path(), SessionMode::Normal)
                .unwrap()
                .unwrap();
            store.save(&fixture()).unwrap();
            let mut large = fixture();
            large.tabs = vec![
                SavedTab {
                    url: format!("https://example.test/{}", "\\".repeat(8000)),
                    pinned: false
                };
                32
            ];
            assert!(store.save(&large).is_err());
            assert_eq!(store.load().unwrap(), Some(fixture()));
        }

        // Phase `malformed_unsafe_and_oversized_snapshots_are_rejected`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = SessionStore::open(root.path(), SessionMode::Normal)
                .unwrap()
                .unwrap();
            let mut cases = vec![fixture(); 5];
            cases[0].version = 999;
            cases[1].tabs = vec![fixture().tabs[0].clone(); 33];
            cases[2].tabs[0].url = "javascript:alert(1)".into();
            cases[3].tabs[0].url = "https://user:secret@host.test/".into();
            cases[4].tabs[0].pinned = false;
            cases[4].tabs[1].pinned = true;
            for value in cases {
                std::fs::write(
                    root.path().join("session.json"),
                    serde_json::to_vec(&value).unwrap(),
                )
                .unwrap();
                assert!(store.load().is_err());
            }
            std::fs::write(root.path().join("session.json"), vec![b'x'; 300_000]).unwrap();
            assert!(store.load().is_err());
            assert!(safe_url("http://127.0.0.1:6111/test"));
            assert!(safe_url("about:blank"));
            assert!(!safe_url("file:///tmp/private"));
            assert!(!safe_url("data:text/plain,private"));
        }

        // Phase `live_instance_lock_and_explicit_private_policy_prevent_writes`.
        {
            let root = tempfile::tempdir().unwrap();
            let private = root.path().join("private");
            assert!(
                SessionStore::open(&private, SessionMode::Private)
                    .unwrap()
                    .is_none()
            );
            assert!(!private.exists());
            let first = SessionStore::open(root.path(), SessionMode::Normal)
                .unwrap()
                .unwrap();
            first.save(&fixture()).unwrap();
            assert!(SessionStore::open(root.path(), SessionMode::Normal).is_err());
            drop(first);
            assert!(
                SessionStore::open(root.path(), SessionMode::Normal)
                    .unwrap()
                    .is_some()
            );
        }
    }
}
