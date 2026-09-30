//! Host-private, single-writer invocation admission. Never replicated.
//!
//! No dedup key is evicted. Any failed write poisons this writer; a restart
//! validates the last atomic snapshot and turns every nonterminal into an
//! uncertain observation. Recovery never returns a dispatch permit.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
#[path = "invocation_journal/guarded.rs"]
mod guarded;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tilo_actions::{InvocationId, InvocationUpdate};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum InvocationOwner {
    Local {
        actor: String,
    },
    Certified {
        root: [u8; 32],
        device: [u8; 32],
    },
    /// Exact admitted local-channel identity, distinct from actor-only legacy
    /// ownership and from certified peer callers. This is retained data only.
    AuthenticatedLocal {
        root: [u8; 32],
        device: [u8; 32],
        #[serde(deserialize_with = "required_agent")]
        agent: Option<String>,
    },
    /// Retained ownership data only; observation still requires live task authority.
    Enrolled {
        root: [u8; 32],
        device: [u8; 32],
        task: String,
        run: String,
        channel: String,
    },
}

// Unlike serde's implicit Option default, an omitted field must not silently
// turn a delegated-agent owner into an ordinary person owner.
fn required_agent<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

impl InvocationOwner {
    pub(super) fn validate(&self) -> Result<(), String> {
        if let Self::AuthenticatedLocal {
            agent: Some(agent), ..
        } = self
            && (agent.is_empty()
                || agent.len() > 512
                || agent.bytes().any(|byte| byte.is_ascii_control()))
        {
            return Err("invalid authenticated local invocation agent".into());
        }
        if let Self::Enrolled {
            task, run, channel, ..
        } = self
        {
            for value in [task, run, channel] {
                let id = uuid::Uuid::parse_str(value)
                    .map_err(|_| "invalid enrolled invocation owner")?;
                if value.len() != 36 || id.is_nil() || id.to_string() != *value {
                    return Err("noncanonical enrolled invocation owner".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InvocationKey {
    pub room: String,
    pub host: uuid::Uuid,
    pub owner: InvocationOwner,
    pub invocation: InvocationId,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DurableInvocation {
    pub(super) key: InvocationKey,
    pub(super) intent: serde_json::Value,
    pub wire_id: u64,
    pub update: InvocationUpdate,
    pub uncertain: bool,
    pub cancellation_requested: bool,
    guarded: Option<guarded::State>,
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
    _lock: advisory_lock::FileLock,
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
        Self::open_with_parent_barrier(root, |parent| {
            File::open(parent).and_then(|file| file.sync_all())
        })
    }

    fn open_with_parent_barrier(
        root: &Path,
        parent_barrier: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<Self, String> {
        Self::open_with_barriers(root, parent_barrier, |file, directory| {
            file.sync_all()?;
            directory.sync_all()
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered open, lock, decode and validation pass over the journal snapshot"
    )]
    fn open_with_barriers(
        root: &Path,
        parent_barrier: impl FnOnce(&Path) -> std::io::Result<()>,
        guarded_barrier: impl FnOnce(&File, &File) -> std::io::Result<()>,
    ) -> Result<Self, String> {
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
        // Retain the directory actually opened for this snapshot's guarded
        // reconciliation, rather than reopening its pathname after decoding.
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(root)
            .map_err(|_| "invocation journal directory unavailable")?;
        let actual_directory = directory
            .metadata()
            .map_err(|_| "invocation journal directory metadata unavailable")?;
        if actual_directory.dev() != metadata.dev() || actual_directory.ino() != metadata.ino() {
            return Err("invocation journal directory changed during open".into());
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
        let lock = advisory_lock::FileLock::try_exclusive(lock)
            .map_err(|_| "invocation journal already has a writer")?;
        let mut observed_snapshot = None;
        let snapshot = if created {
            Snapshot {
                version: 1,
                host: uuid::Uuid::new_v4(),
                records: Vec::new(),
            }
        } else {
            let mut file = OpenOptions::new()
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
            guarded::preflight(&mut file)?;
            file.seek(SeekFrom::Start(0))
                .map_err(|_| "invocation journal rewind failed")?;
            let snapshot =
                serde_json::from_reader(std::io::BufReader::new((&mut file).take(Self::MAX_BYTES)))
                    .map_err(|_| "invocation journal snapshot is corrupt or unsupported")?;
            observed_snapshot = Some(file);
            snapshot
        };
        if !matches!(snapshot.version, 1..=4) || snapshot.host.is_nil() {
            return Err("invocation journal version or host unsupported".into());
        }
        let mut keys = HashSet::new();
        let mut handles = HashSet::new();
        let mut invocations = HashSet::new();
        for record in &snapshot.records {
            record.key.owner.validate()?;
            if record.guarded.is_some() && snapshot.version < 3 {
                return Err("guarded read requires journal version 3".into());
            }
            if snapshot.version < 4
                && matches!(record.key.owner, InvocationOwner::AuthenticatedLocal { .. })
            {
                return Err("authenticated local ownership requires journal version 4".into());
            }
            if snapshot.version == 1 && matches!(record.key.owner, InvocationOwner::Enrolled { .. })
            {
                return Err("private invocation ownership requires journal version 2".into());
            }
            if record.key.host != snapshot.host
                || record.key.invocation != record.update.invocation()
                || record.wire_id == 0
                || !keys.insert(record.key.clone())
                || !handles.insert(record.wire_id)
                || !invocations.insert(record.key.invocation)
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
        } else if journal
            .snapshot
            .records
            .iter()
            .any(|record| record.guarded.is_some())
        {
            // A visible terminal marker may be the result of a failed rename
            // directory barrier. Reconcile the exact decoded file, not a
            // freshly reopened name, before exposing even sanitized status.
            guarded_barrier(
                observed_snapshot
                    .as_ref()
                    .ok_or("guarded snapshot unavailable")?,
                &directory,
            )
            .map_err(|_| "guarded invocation snapshot reconciliation failed")?;
        }
        // A failed initialization may leave a valid snapshot. Its existence
        // does not prove the directory entry was durable: retry on every open.
        parent_barrier(root.parent().ok_or("invocation journal needs a parent")?)
            .map_err(|_| "invocation journal parent barrier failed")?;
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
        key.owner.validate()?;
        if let Some(existing) = self.lookup(&key)? {
            if existing.intent != intent {
                return Err("invocation identity reused with different intent".into());
            }
            return Ok(false);
        }
        // Application lifecycle routing and internal dedup use the full UUID
        // without the owner. Permanently reserve it for one durable key so a
        // different device, room, or legacy caller can never alias that state.
        if self
            .snapshot
            .records
            .iter()
            .any(|r| r.key.invocation == key.invocation)
        {
            return Err("action_invocation_identity_conflict: UUID already reserved".into());
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
        if matches!(key.owner, InvocationOwner::Enrolled { .. }) {
            self.snapshot.version = self.snapshot.version.max(2);
        }
        if matches!(key.owner, InvocationOwner::AuthenticatedLocal { .. }) {
            self.snapshot.version = self.snapshot.version.max(4);
        }
        self.snapshot.records.push(DurableInvocation {
            key,
            intent,
            wire_id,
            update,
            uncertain: false,
            cancellation_requested: false,
            guarded: None,
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
        if record.guarded.is_some() && !guarded::allowed_failure(&update, uncertain) {
            return Err(
                "guarded invocation requires a sanitized failure or private terminal".into(),
            );
        }
        record.update = update;
        record.uncertain = uncertain;
        self.persist()?;
        Ok(())
    }

    // Storage prerequisites only. No production caller is wired until the
    // genuine selected worker/owner capsule protocol is installed.
    #[cfg(test)]
    fn require_guarded_read(
        &mut self,
        key: &InvocationKey,
        selection: [u8; 32],
    ) -> Result<(), String> {
        self.check()?;
        let record = self
            .snapshot
            .records
            .iter_mut()
            .find(|r| &r.key == key)
            .ok_or("unknown invocation journal identity")?;
        if let Some(guarded) = &record.guarded {
            return if guarded.selection() == &selection {
                Ok(())
            } else {
                Err("changed guarded selection".into())
            };
        }
        if !matches!(record.update, InvocationUpdate::Accepted { .. })
            || record.uncertain
            || record.cancellation_requested
        {
            return Err("guarded selection requires fresh accepted invocation".into());
        }
        record.guarded = Some(guarded::State::selected(selection));
        self.snapshot.version = self.snapshot.version.max(3);
        self.persist()
    }

    #[cfg(test)]
    fn complete_guarded_read(
        &mut self,
        key: &InvocationKey,
        selection: [u8; 32],
        output: [u8; 32],
        basis: [u8; 32],
        bytes: u32,
    ) -> Result<(), String> {
        self.check()?;
        let record = self
            .snapshot
            .records
            .iter_mut()
            .find(|r| &r.key == key)
            .ok_or("unknown invocation journal identity")?;
        if !matches!(record.update, InvocationUpdate::Accepted { .. })
            || record.uncertain
            || record.cancellation_requested
        {
            return Err("guarded terminal requires live accepted invocation".into());
        }
        let guarded = record.guarded.as_mut().ok_or("guarded selection absent")?;
        if guarded.selection() != &selection {
            return Err("changed guarded selection".into());
        }
        guarded.complete(key.invocation, output, basis, bytes)?;
        record.update = guarded::unavailable(key.invocation);
        record.uncertain = false;
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
        if record.guarded.is_some() {
            return Err("guarded reads are noncancellable".into());
        }
        record.cancellation_requested = true;
        self.persist()
    }

    pub(super) fn check(&self) -> Result<(), String> {
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

    fn write_snapshot(&self) -> Result<(), String> {
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
        error: tilo_actions::ActionFailure {
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

    const LEGACY_SNAPSHOT: &str = r#"{"version":1,"host":"00000000-0000-0000-0000-000000000063","records":[{"key":{"room":"00000000-0000-0000-0000-000000000001","host":"00000000-0000-0000-0000-000000000063","owner":{"Local":{"actor":"person"}},"invocation":"00000000-0000-0000-0000-000000000004"},"intent":{},"wire_id":1,"update":{"state":"succeeded","invocation":"00000000-0000-0000-0000-000000000004","output":{"value":"legacy"}},"uncertain":false,"cancellation_requested":false}]}"#;
    const ENROLLED_SNAPSHOT: &str = r#"{"version":2,"host":"00000000-0000-0000-0000-000000000063","records":[{"key":{"room":"00000000-0000-0000-0000-000000000001","host":"00000000-0000-0000-0000-000000000063","owner":{"Enrolled":{"root":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"device":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"task":"00000000-0000-0000-0000-000000000005","run":"00000000-0000-0000-0000-000000000006","channel":"00000000-0000-0000-0000-000000000007"}},"invocation":"00000000-0000-0000-0000-000000000004"},"intent":{},"wire_id":1,"update":{"state":"succeeded","invocation":"00000000-0000-0000-0000-000000000004","output":{"value":"private result"}},"uncertain":false,"cancellation_requested":false}]}"#;

    #[test]
    fn enrolled_journal_literal_versions_retain_owner_and_writer_floor() {
        for (literal, version) in [(LEGACY_SNAPSHOT, 1), (ENROLLED_SNAPSHOT, 2)] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("journal");
            drop(InvocationJournal::open(&root).unwrap());
            std::fs::write(root.join("records.json"), literal).unwrap();
            let mut journal = InvocationJournal::open(&root).expect("supported private ownership");
            let before: serde_json::Value = serde_json::from_str(literal).unwrap();
            assert_eq!(serde_json::to_value(&journal.snapshot).unwrap(), before);
            let mut local = key();
            local.host = journal.host();
            local.owner = InvocationOwner::Local {
                actor: "person".into(),
            };
            journal.admit(local, serde_json::json!({}), 2).unwrap();
            assert_eq!(
                journal.snapshot.version, version,
                "legacy writes never downgrade a private store"
            );
            drop(journal);
            let recovered = InvocationJournal::open(&root).unwrap();
            assert_eq!(
                serde_json::to_value(&recovered.snapshot.records[0]).unwrap(),
                before["records"][0]
            );
            if version == 2 {
                let mut foreign = recovered.snapshot.records[0].key.clone();
                for owner in [
                    InvocationOwner::Local {
                        actor: "person".into(),
                    },
                    InvocationOwner::Certified {
                        root: [1; 32],
                        device: [2; 32],
                    },
                    InvocationOwner::Certified {
                        root: [1; 32],
                        device: [3; 32],
                    },
                ] {
                    foreign.owner = owner;
                    assert!(
                        recovered.lookup(&foreign).unwrap().is_none(),
                        "private recovered result cannot alias a public owner"
                    );
                }
            }
        }
    }

    #[test]
    fn enrolled_journal_old_owner_decoder_rejects_private_rows() {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        enum VersionOneOwner {
            Local { actor: String },
            Certified { root: [u8; 32], device: [u8; 32] },
        }
        let legacy: serde_json::Value = serde_json::from_str(LEGACY_SNAPSHOT).unwrap();
        let old =
            serde_json::from_value::<VersionOneOwner>(legacy["records"][0]["key"]["owner"].clone())
                .unwrap();
        assert!(matches!(old, VersionOneOwner::Local { actor } if actor == "person"));
        let certified: VersionOneOwner = serde_json::from_value(
            serde_json::to_value(InvocationOwner::Certified {
                root: [1; 32],
                device: [2; 32],
            })
            .unwrap(),
        )
        .unwrap();
        assert!(
            matches!(certified, VersionOneOwner::Certified { root, device } if root == [1;32] && device == [2;32])
        );
        let private: serde_json::Value = serde_json::from_str(ENROLLED_SNAPSHOT).unwrap();
        assert_ne!(private["version"], 1, "old snapshot gate fails explicitly");
        assert!(
            serde_json::from_value::<VersionOneOwner>(
                private["records"][0]["key"]["owner"].clone()
            )
            .is_err(),
            "old enum never silently classifies private ownership as Local"
        );
    }

    #[test]
    fn enrolled_journal_upgrade_barrier_never_returns_effect_permission_on_failure() {
        let private: Snapshot =
            serde_json::from_str(ENROLLED_SNAPSHOT).expect("private owner data");
        for failure in [BarrierFailure::File, BarrierFailure::Directory] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("journal");
            let mut journal = InvocationJournal::open(&root).unwrap();
            let mut key = private.records[0].key.clone();
            key.host = journal.host();
            journal.fail_at = Some(failure);
            assert!(
                journal
                    .admit(key.clone(), serde_json::json!({}), 1)
                    .is_err()
            );
            assert!(
                journal
                    .admit(key.clone(), serde_json::json!({}), 2)
                    .is_err()
            );
            drop(journal);
            let reopened = InvocationJournal::open(&root).unwrap();
            if failure == BarrierFailure::Directory {
                assert_eq!(reopened.snapshot.version, 2);
                let retained = reopened.lookup(&key).unwrap().unwrap();
                assert!(retained.uncertain);
                assert_eq!(retained.key.owner, key.owner);
            } else {
                assert_eq!(reopened.snapshot.version, 1);
                assert!(reopened.lookup(&key).unwrap().is_none());
            }
        }
    }

    #[test]
    fn enrolled_journal_rejects_private_downgrade_and_noncanonical_ownership() {
        for literal in [
            ENROLLED_SNAPSHOT.replace("\"version\":2", "\"version\":1"),
            ENROLLED_SNAPSHOT.replace(
                "00000000-0000-0000-0000-000000000005",
                "00000000000000000000000000000005",
            ),
            ENROLLED_SNAPSHOT.replace(
                "00000000-0000-0000-0000-000000000006",
                "00000000-0000-0000-0000-000000000000",
            ),
            ENROLLED_SNAPSHOT.replace("00000000-0000-0000-0000-000000000007", &"x".repeat(200)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("journal");
            drop(InvocationJournal::open(&root).unwrap());
            std::fs::write(root.join("records.json"), literal).unwrap();
            assert!(InvocationJournal::open(&root).is_err());
        }
    }

    fn key() -> InvocationKey {
        InvocationKey {
            room: "5ca720b0-7649-4bdc-9c30-77380f7c2be5".into(),
            host: uuid::Uuid::from_u128(99),
            owner: InvocationOwner::Certified {
                root: [1; 32],
                device: [2; 32],
            },
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(
                0xa5a5_a5a5_0000_0000_0000_0000_0000_0001,
            )),
        }
    }

    #[test]
    fn invocation_journal_initialization_parent_barrier_must_succeed_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let attempts = std::cell::Cell::new(0);
        for expected in 1..=2 {
            let opened = InvocationJournal::open_with_parent_barrier(&root, |parent| {
                assert_eq!(parent, dir.path());
                attempts.set(attempts.get() + 1);
                Err(std::io::Error::other("injected parent sync failure"))
            });
            assert!(
                opened.is_err(),
                "no writer or dispatch permit before parent sync"
            );
            assert_eq!(
                attempts.get(),
                expected,
                "reopen retries the failed barrier"
            );
            assert!(
                root.join("records.json").is_file(),
                "failure leaves a valid store"
            );
        }
        let mut journal = InvocationJournal::open_with_parent_barrier(&root, |parent| {
            attempts.set(attempts.get() + 1);
            File::open(parent)?.sync_all()
        })
        .unwrap();
        assert_eq!(attempts.get(), 3);
        let mut k = key();
        k.host = journal.host();
        assert!(journal.admit(k, serde_json::json!({}), 1).unwrap());
    }

    #[test]
    fn invocation_journal_rejects_preexisting_cross_owner_uuid_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let mut journal = InvocationJournal::open(&root).unwrap();
        let mut k = key();
        k.host = journal.host();
        journal.admit(k, serde_json::json!({}), 1).unwrap();
        let mut conflicting = journal.snapshot.records[0].clone();
        conflicting.key.owner = InvocationOwner::Local {
            actor: "other".into(),
        };
        conflicting.wire_id = 2;
        journal.snapshot.records.push(conflicting);
        journal.persist().unwrap();
        drop(journal);
        assert!(
            InvocationJournal::open(&root).is_err(),
            "an older ambiguous store cannot authorize either application UUID owner"
        );
    }

    #[test]
    fn invocation_journal_restart_retains_terminal_and_never_reissues_admission() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let mut journal = InvocationJournal::open(&root).unwrap();
        let mut k = key();
        k.host = journal.host();
        let intent = serde_json::json!({"argument": null, "action": "write"});
        let update = tilo_actions::InvocationUpdate::Succeeded {
            invocation: k.invocation,
            output: tilo_actions::ActionOutput {
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
            matches!(record.update, tilo_actions::InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown" && !error.retryable)
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
        const PROBE: &str = "TILO_INVOCATION_JOURNAL_LOCK_PROBE";
        if let Some(root) = std::env::var_os(PROBE) {
            assert!(InvocationJournal::open(Path::new(&root)).is_err());
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let _journal = InvocationJournal::open(&root).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg("collab_broker_capnp::invocation_journal::tests::invocation_journal_excludes_a_second_process")
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

#[cfg(test)]
#[path = "invocation_journal/guarded_tests.rs"]
mod guarded_tests;

#[cfg(test)]
#[path = "invocation_journal/authenticated_local_tests.rs"]
mod authenticated_local_tests;
