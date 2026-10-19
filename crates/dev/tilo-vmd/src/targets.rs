//! In-VM build dirs: one synced source tree per host worktree under
//! `<root>/src/<worktree>/`, whose `target/` is that worktree's own cargo
//! build dir. Build dirs are never shared and are disposable: `gc` evicts
//! them least recently used first. The shared cache is sccache.
//!
//! ```text
//! src/<worktree>/          rsync of the worktree (respects .gitignore)
//! src/<worktree>/target/   its cargo target dir (rsync never touches it)
//! src/<worktree>.json      owner, last use, bytes after the last build
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_vm_protocol::Owner;

use crate::error::DaemonError;
use crate::fsutil::{self, write_atomic};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetRecord {
    pub worktree: String,
    #[serde(default)]
    pub owner: Owner,
    /// RFC 3339 time of the last build start.
    pub last_used: String,
    /// Seconds since the epoch of the last build start, for ordering.
    pub last_used_unix: u64,
    /// Allocated bytes of `target/` after the last build.
    #[serde(default)]
    pub bytes: u64,
}

pub struct TargetStore {
    dir: PathBuf,
}

/// Worktree ids become directory names: short, lowercase, dash-safe.
pub fn valid_worktree_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

impl TargetStore {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: root.join("src"),
        }
    }

    /// The synced source tree of `worktree`.
    pub fn src(&self, worktree: &str) -> PathBuf {
        self.dir.join(worktree)
    }

    /// The build root: every synced tree and its record.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn record_path(&self, worktree: &str) -> PathBuf {
        self.dir.join(format!("{worktree}.json"))
    }

    /// Mark `worktree` as used now; creates the record on first use.
    ///
    /// # Errors
    /// Invalid ids and I/O failures.
    pub fn touch(&self, worktree: &str, owner: Owner) -> Result<TargetRecord, DaemonError> {
        if !valid_worktree_id(worktree) {
            return Err(DaemonError::InvalidParams(format!(
                "'{worktree}' is not a valid worktree id"
            )));
        }
        let previous = self.load(worktree);
        let record = TargetRecord {
            worktree: worktree.to_owned(),
            owner,
            last_used: crate::slots::now(),
            last_used_unix: tilo_clock::unix_now(),
            bytes: previous.map_or(0, |p| p.bytes),
        };
        self.save(&record)?;
        Ok(record)
    }

    /// Measure `target/` and record its size.
    ///
    /// # Errors
    /// I/O failures writing the record.
    pub fn measure(&self, worktree: &str) -> Result<u64, DaemonError> {
        let bytes = fsutil::allocated_bytes(&self.src(worktree).join("target"));
        if let Some(mut record) = self.load(worktree) {
            record.bytes = bytes;
            self.save(&record)?;
        }
        Ok(bytes)
    }

    pub fn load(&self, worktree: &str) -> Option<TargetRecord> {
        let bytes = fs::read(self.record_path(worktree)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn save(&self, record: &TargetRecord) -> Result<(), DaemonError> {
        let bytes = serde_json::to_vec_pretty(record)
            .map_err(|e| DaemonError::InvalidParams(e.to_string()))?;
        write_atomic(&self.record_path(&record.worktree), &bytes)
    }

    /// Every recorded build dir, most recently used first.
    pub fn list(&self) -> Vec<TargetRecord> {
        let mut records: Vec<TargetRecord> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                self.load(name.strip_suffix(".json")?)
            })
            .collect();
        records.sort_by(|a, b| {
            b.last_used_unix
                .cmp(&a.last_used_unix)
                .then_with(|| a.worktree.cmp(&b.worktree))
        });
        records
    }

    /// Recorded bytes of every build dir.
    pub fn total_bytes(&self) -> u64 {
        self.list()
            .iter()
            .map(|r| r.bytes)
            .fold(0, u64::saturating_add)
    }

    /// Which build dirs to evict: beyond the `keep` most recent, and then,
    /// oldest first, as many more as it takes to reach `free_goal` given
    /// `free` bytes free now. `busy` dirs (a build is running) are skipped.
    pub fn plan_eviction(
        &self,
        keep: u32,
        free_goal: Option<u64>,
        free: Option<u64>,
        busy: &mut dyn FnMut(&str) -> bool,
    ) -> Vec<TargetRecord> {
        let keep = usize::try_from(keep).unwrap_or(usize::MAX);
        let mut records = self.list();
        records.retain(|r| r.bytes > 0 || self.src(&r.worktree).join("target").exists());
        let mut free = free.unwrap_or(u64::MAX);
        let mut evict = Vec::new();
        // Oldest first.
        for (index, record) in records.into_iter().enumerate().rev() {
            let beyond_keep = index >= keep;
            let short_of_goal = free_goal.is_some_and(|goal| free < goal);
            if !(beyond_keep || short_of_goal) || busy(&record.worktree) {
                continue;
            }
            free = free.saturating_add(record.bytes);
            evict.push(record);
        }
        evict
    }

    /// Delete a worktree's `target/` (its synced sources stay).
    ///
    /// # Errors
    /// I/O failures.
    pub fn evict(&self, worktree: &str) -> Result<(), DaemonError> {
        let target = self.src(worktree).join("target");
        if target.exists() {
            let doomed = self.dir.join(format!(
                ".evict-{worktree}-{}",
                uuid::Uuid::new_v4().simple()
            ));
            fs::rename(&target, &doomed)
                .map_err(DaemonError::io(format!("evict build dir of {worktree}")))?;
            fs::remove_dir_all(&doomed)
                .map_err(DaemonError::io(format!("evict build dir of {worktree}")))?;
        }
        if let Some(mut record) = self.load(worktree) {
            record.bytes = 0;
            self.save(&record)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(root: &Path, entries: &[(&str, u64, u64)]) -> TargetStore {
        let store = TargetStore::new(root);
        for (id, used, bytes) in entries {
            fs::create_dir_all(store.src(id).join("target/release")).unwrap();
            fs::write(store.src(id).join("target/release/shell"), b"x").unwrap();
            store
                .save(&TargetRecord {
                    worktree: (*id).into(),
                    owner: Owner::default(),
                    last_used: String::new(),
                    last_used_unix: *used,
                    bytes: *bytes,
                })
                .unwrap();
        }
        store
    }

    #[test]
    fn build_targets_are_directory_safe_and_evicted_least_recently_used() {
        // Phase `ids_are_directory_safe`.
        {
            assert!(valid_worktree_id("agent-a74-3f9c1a2b"));
            assert!(!valid_worktree_id("../etc"));
            assert!(!valid_worktree_id("Upper"));
            assert!(!valid_worktree_id(""));
            assert!(!valid_worktree_id(&"a".repeat(65)));
        }
        // Phase `eviction_is_least_recently_used_first`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = store_with(
                root.path(),
                &[("old", 100, 10), ("mid", 200, 20), ("new", 300, 30)],
            );
            assert_eq!(
                store
                    .list()
                    .iter()
                    .map(|r| r.worktree.as_str())
                    .collect::<Vec<_>>(),
                ["new", "mid", "old"]
            );
            assert_eq!(store.total_bytes(), 60);
            let names = |records: Vec<TargetRecord>| {
                records.into_iter().map(|r| r.worktree).collect::<Vec<_>>()
            };
            // Keep two: the oldest goes.
            assert_eq!(
                names(store.plan_eviction(2, None, None, &mut |_| false)),
                ["old"]
            );
            // A free-space goal takes more, oldest first, until it is met.
            assert_eq!(
                names(store.plan_eviction(3, Some(35), Some(10), &mut |_| false)),
                ["old", "mid"]
            );
            // A running build is never evicted.
            assert_eq!(
                names(store.plan_eviction(0, None, None, &mut |id| id == "mid")),
                ["old", "new"]
            );
            store.evict("old").unwrap();
            assert!(!store.src("old").join("target").exists());
            assert!(store.src("old").exists(), "sources stay");
            assert_eq!(store.load("old").unwrap().bytes, 0);
            assert_eq!(
                names(store.plan_eviction(0, None, None, &mut |_| false)),
                ["mid", "new"]
            );
        }
        // Phase `touch_keeps_the_measured_size`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = TargetStore::new(root.path());
            store.touch("wt-1", Owner::default()).unwrap();
            fs::create_dir_all(store.src("wt-1").join("target")).unwrap();
            fs::write(store.src("wt-1").join("target/big"), vec![1_u8; 8192]).unwrap();
            let bytes = store.measure("wt-1").unwrap();
            assert!(bytes >= 8192);
            let again = store.touch("wt-1", Owner::default()).unwrap();
            assert_eq!(again.bytes, bytes);
            assert!(store.touch("../x", Owner::default()).is_err());
        }
    }
}
