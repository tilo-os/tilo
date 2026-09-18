//! Host-private, single-writer invocation admission. Never replicated.
//!
//! No dedup key is evicted. Any failed write poisons this writer; a restart
//! validates the last atomic snapshot and turns every nonterminal into an
//! uncertain observation. Recovery never returns a dispatch permit.

use qos_actions::{InvocationId, InvocationUpdate};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum InvocationOwner {
    Local { actor: String },
    Certified { root: [u8; 32], device: [u8; 32] },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InvocationKey {
    pub room: String,
    pub host: uuid::Uuid,
    pub owner: InvocationOwner,
    pub invocation: InvocationId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DurableInvocation {
    pub(super) key: InvocationKey,
    pub(super) intent: serde_json::Value,
    pub wire_id: u64,
    pub update: InvocationUpdate,
    pub uncertain: bool,
    pub cancellation_requested: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    host: uuid::Uuid,
    records: Vec<DurableInvocation>,
}

pub(super) struct InvocationJournal {
    root: PathBuf,
    _lock: File,
    snapshot: Snapshot,
    poisoned: bool,
    #[cfg(test)]
    pub fail_at: Option<BarrierFailure>,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BarrierFailure {
    File,
    Directory,
    AfterAdmission,
    Terminal,
}

impl InvocationJournal {
    const MAX_BYTES: u64 = 256 * 1024 * 1024;

    pub fn open(root: &Path) -> Result<Self, String> {
        let created = match std::fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(_) => return Err("invocation journal directory unavailable".into()),
        };
        let metadata = std::fs::symlink_metadata(root)
            .map_err(|_| "invocation journal directory unreadable")?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
            return Err("invocation journal requires a private directory".into());
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join("owner.lock"))
            .map_err(|_| "invocation journal lock unavailable")?;
        private_file(&lock)?;
        lock.try_lock()
            .map_err(|_| "invocation journal already has a writer")?;
        let snapshot = if created {
            Snapshot {
                version: 1,
                host: uuid::Uuid::new_v4(),
                records: Vec::new(),
            }
        } else {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(root.join("records.json"))
                .map_err(|_| "invocation journal snapshot missing or unreadable")?;
            private_file(&file)?;
            if file
                .metadata()
                .map_err(|_| "invocation journal metadata unreadable")?
                .len()
                > Self::MAX_BYTES
            {
                return Err("invocation journal exceeds storage limit".into());
            }
            serde_json::from_reader(file.take(Self::MAX_BYTES))
                .map_err(|_| "invocation journal snapshot is corrupt or unsupported")?
        };
        if snapshot.version != 1 || snapshot.host.is_nil() {
            return Err("invocation journal version or host unsupported".into());
        }
        let mut keys = HashSet::new();
        let mut handles = HashSet::new();
        for record in &snapshot.records {
            if record.key.host != snapshot.host
                || record.key.invocation != record.update.invocation()
                || record.wire_id == 0
                || !keys.insert(record.key.clone())
                || !handles.insert(record.wire_id)
            {
                return Err("invocation journal contains inconsistent identities".into());
            }
        }
        let mut journal = Self {
            root: root.to_owned(),
            _lock: lock,
            snapshot,
            poisoned: false,
            #[cfg(test)]
            fail_at: None,
        };
        let mut recovered = false;
        for record in &mut journal.snapshot.records {
            if !record.update.is_terminal() {
                record.update = unknown(record.key.invocation);
                record.uncertain = true;
                recovered = true;
            }
        }
        if created || recovered {
            journal.persist()?;
        }
        if created {
            File::open(root.parent().ok_or("invocation journal needs a parent")?)
                .and_then(|file| file.sync_all())
                .map_err(|_| "invocation journal parent barrier failed")?;
        }
        Ok(journal)
    }

    pub fn host(&self) -> uuid::Uuid {
        self.snapshot.host
    }
    pub fn next_wire_id(&self) -> u64 {
        self.snapshot
            .records
            .iter()
            .map(|r| r.wire_id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    pub fn lookup(&self, key: &InvocationKey) -> Result<Option<DurableInvocation>, String> {
        self.check()?;
        Ok(self
            .snapshot
            .records
            .iter()
            .find(|r| &r.key == key)
            .cloned())
    }

    /// A true return is the sole dispatch permit. Caller must serialize the
    /// authorization / confirmation / admission / dispatch transaction.
    pub fn admit(
        &mut self,
        key: InvocationKey,
        intent: serde_json::Value,
        wire_id: u64,
    ) -> Result<bool, String> {
        self.check()?;
        if let Some(existing) = self.lookup(&key)? {
            if existing.intent != intent {
                return Err("invocation identity reused with different intent".into());
            }
            return Ok(false);
        }
        if key.host != self.host()
            || wire_id == 0
            || self.snapshot.records.iter().any(|r| r.wire_id == wire_id)
        {
            return Err("invocation journal identity conflict".into());
        }
        let update = InvocationUpdate::Accepted {
            invocation: key.invocation,
        };
        self.snapshot.records.push(DurableInvocation {
            key,
            intent,
            wire_id,
            update,
            uncertain: false,
            cancellation_requested: false,
        });
        self.persist()?;
        #[cfg(test)]
        if self.fail_at.take() == Some(BarrierFailure::AfterAdmission) {
            self.poisoned = true;
            return Err("injected admission-before-delivery crash".into());
        }
        Ok(true)
    }

    pub fn update(
        &mut self,
        key: &InvocationKey,
        update: InvocationUpdate,
        uncertain: bool,
    ) -> Result<(), String> {
        self.check()?;
        let record = self
            .snapshot
            .records
            .iter_mut()
            .find(|r| &r.key == key)
            .ok_or("unknown invocation journal identity")?;
        if record.update.is_terminal() || update.invocation() != key.invocation {
            return Err("invocation journal transition rejected".into());
        }
        record.update = update;
        record.uncertain = uncertain;
        self.persist()
    }

    pub fn cancel(&mut self, key: &InvocationKey) -> Result<(), String> {
        self.check()?;
        let record = self
            .snapshot
            .records
            .iter_mut()
            .find(|r| &r.key == key)
            .ok_or("unknown invocation journal identity")?;
        if record.update.is_terminal() {
            return Err("invocation is no longer running".into());
        }
        record.cancellation_requested = true;
        self.persist()
    }

    fn check(&self) -> Result<(), String> {
        if self.poisoned {
            Err("invocation journal requires recovery after a failed barrier".into())
        } else {
            Ok(())
        }
    }

    fn persist(&mut self) -> Result<(), String> {
        let outcome = self.write_snapshot();
        if outcome.is_err() {
            self.poisoned = true;
        }
        outcome
    }

    fn write_snapshot(&mut self) -> Result<(), String> {
        let bytes =
            serde_json::to_vec(&self.snapshot).map_err(|_| "invocation journal encoding failed")?;
        if bytes.len() as u64 > Self::MAX_BYTES {
            return Err("invocation journal storage limit reached".into());
        }
        let temporary = self.root.join(format!(".write-{}", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temporary)
                .map_err(|_| "invocation journal temporary file unavailable")?;
            file.write_all(&bytes)
                .map_err(|_| "invocation journal write failed")?;
            #[cfg(test)]
            if self.fail_at == Some(BarrierFailure::File) {
                return Err("injected file barrier failure");
            }
            file.sync_all()
                .map_err(|_| "invocation journal file barrier failed")?;
            std::fs::rename(&temporary, self.root.join("records.json"))
                .map_err(|_| "invocation journal rename failed")?;
            #[cfg(test)]
            if self.fail_at == Some(BarrierFailure::Directory) {
                return Err("injected directory barrier failure");
            }
            File::open(&self.root)
                .and_then(|file| file.sync_all())
                .map_err(|_| "invocation journal directory barrier failed")?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&temporary);
        result.map_err(str::to_string)
    }
}

fn private_file(file: &File) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|_| "invocation journal file unreadable")?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
        return Err("invocation journal requires private regular files".into());
    }
    Ok(())
}

pub(super) fn unknown(invocation: InvocationId) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: qos_actions::ActionFailure {
            code: "action_outcome_unknown".into(),
            message:
                "The application outcome is unknown; this invocation will not be executed again."
                    .into(),
            details: None,
            retryable: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> InvocationKey {
        InvocationKey {
            room: "5ca720b0-7649-4bdc-9c30-77380f7c2be5".into(),
            host: uuid::Uuid::from_u128(99),
            owner: InvocationOwner::Certified {
                root: [1; 32],
                device: [2; 32],
            },
            invocation: qos_actions::InvocationId(uuid::Uuid::from_u128(
                0xa5a5_a5a5_0000_0000_0000_0000_0000_0001,
            )),
        }
    }

    #[test]
    fn invocation_journal_restart_retains_terminal_and_never_reissues_admission() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let mut journal = InvocationJournal::open(&root).unwrap();
        let mut k = key();
        k.host = journal.host();
        let intent = serde_json::json!({"argument": null, "action": "write"});
        let update = qos_actions::InvocationUpdate::Succeeded {
            invocation: k.invocation,
            output: qos_actions::ActionOutput {
                value: Some(serde_json::json!({"private": "result"})),
                resources: vec![],
            },
        };
        assert!(journal.admit(k.clone(), intent.clone(), 1).unwrap());
        assert!(!journal.admit(k.clone(), intent.clone(), 1).unwrap());
        assert!(
            journal
                .admit(
                    k.clone(),
                    serde_json::json!({"argument":"", "action":"write"}),
                    2
                )
                .is_err()
        );
        journal.update(&k, update.clone(), false).unwrap();
        drop(journal);
        let mut journal = InvocationJournal::open(&root).unwrap();
        assert!(!journal.admit(k.clone(), intent, 2).unwrap());
        assert_eq!(journal.lookup(&k).unwrap().unwrap().update, update);
    }

    #[test]
    fn invocation_journal_recovery_of_admission_is_uncertain_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let mut journal = InvocationJournal::open(&root).unwrap();
        let mut k = key();
        k.host = journal.host();
        journal
            .admit(k.clone(), serde_json::json!({"argument": "secret"}), 1)
            .unwrap();
        drop(journal);
        let journal = InvocationJournal::open(&root).unwrap();
        let record = journal.lookup(&k).unwrap().unwrap();
        assert!(record.uncertain);
        assert!(
            matches!(record.update, qos_actions::InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown" && !error.retryable)
        );
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(root.join("records.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn invocation_journal_lock_and_invalid_stores_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let journal = InvocationJournal::open(&root).unwrap();
        assert!(
            InvocationJournal::open(&root).is_err(),
            "second process/file owner must be excluded"
        );
        drop(journal);
        std::fs::write(root.join("records.json"), b"{broken").unwrap();
        assert!(InvocationJournal::open(&root).is_err());
        std::fs::write(
            root.join("records.json"),
            br#"{"version":999,"host":"00000000-0000-0000-0000-000000000063","records":[]}"#,
        )
        .unwrap();
        assert!(InvocationJournal::open(&root).is_err());
    }

    #[test]
    fn invocation_journal_excludes_a_second_process() {
        const PROBE: &str = "QOS_INVOCATION_JOURNAL_LOCK_PROBE";
        if let Some(root) = std::env::var_os(PROBE) {
            assert!(InvocationJournal::open(Path::new(&root)).is_err());
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let _journal = InvocationJournal::open(&root).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg("shell::collab_broker_capnp::invocation_journal::tests::invocation_journal_excludes_a_second_process")
            .env(PROBE, &root).output().unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
    }

    #[test]
    fn invocation_journal_directory_barrier_failure_never_reopens_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let mut journal = InvocationJournal::open(&root).unwrap();
        journal.fail_at = Some(BarrierFailure::Directory);
        let mut k = key();
        k.host = journal.host();
        assert!(
            journal
                .admit(k.clone(), serde_json::json!({"argument": "secret"}), 1)
                .is_err()
        );
        assert!(
            journal
                .admit(k.clone(), serde_json::json!({"argument": "secret"}), 2)
                .is_err(),
            "poisoned writer cannot retry"
        );
        drop(journal);
        let journal = InvocationJournal::open(&root).unwrap();
        assert!(
            journal.lookup(&k).unwrap().unwrap().uncertain,
            "renamed admission survives but never creates another permit"
        );
    }
}
