//! Versioned private confirmation custody, serialized under the room writer lock.
//! Keep the prior version until public commit succeeds: a crash must not destroy
//! the body needed to hydrate the still-current canonical confirmation.
use super::GovernedRoom;
use std::collections::BTreeMap;
use tilo_desktop_model::WorkspaceTaskConfirmation as Confirmation;
use tilo_joined_room::PrivateTaskConfirmationStore as Store;
use tilo_peer_state::workspace_task::{self, WorkspaceTaskSnapshot};

type Versions = BTreeMap<String, Confirmation>;

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct History {
    #[serde(default)]
    confirmations: BTreeMap<String, Vec<Confirmation>>,
    #[serde(default)]
    audit: BTreeMap<String, Vec<serde_json::Value>>,
}

fn version_key(task: &str, public: &Confirmation) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(task, public)).map_err(|e| e.to_string())?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

impl GovernedRoom {
    pub(super) fn validate_confirmation_archives(&self) -> Result<(), String> {
        let history: History = super::private_tasks::read_private(
            &self
                .path
                .with_extension("private-task-confirmation-history-v1.json"),
        )?
        .unwrap_or_default();
        Store::validate_archives(&history.confirmations, &history.audit)
    }

    /// Write ahead both current custody and exact historical archives. Archive
    /// values never enter task policy or the invocation admission ledger.
    pub(super) fn persist_migrated_task_confirmations(
        &self,
        migrated: &BTreeMap<String, tilo_peer_state::workspace_task::NativeTaskPrivateText>,
    ) -> Result<(), String> {
        let path = self
            .path
            .with_extension("private-task-confirmation-history-v1.json");
        let mut history: History = super::private_tasks::read_private(&path)?.unwrap_or_default();
        Store::validate_archives(&history.confirmations, &history.audit)?;
        let mut current = BTreeMap::new();
        for (task, text) in migrated {
            if let Some(full) = &text.confirmation {
                current.insert(task.clone(), full.clone());
            }
            let archive = history.confirmations.entry(task.clone()).or_default();
            for full in &text.historical_confirmations {
                if !archive.contains(full) {
                    archive.push(full.clone());
                }
            }
            let audit = history.audit.entry(task.clone()).or_default();
            for event in &text.historical_confirmation_audit {
                if !audit.contains(event) {
                    audit.push(event.clone());
                }
            }
        }
        Store::validate_archives(&history.confirmations, &history.audit)?;
        let bytes = serde_json::to_vec(&history).map_err(|e| e.to_string())?;
        tilo_files::native::atomic::write_private(&path, &bytes)
            .map_err(|_| "private task confirmation history persistence failed")?;
        self.persist_task_confirmations(&Store::from_confirmations(current)?)
    }

    pub(super) fn validate_migrated_task_confirmations(
        &self,
        doc: &automerge::AutoCommit,
    ) -> Result<(), String> {
        let versions = self.confirmation_versions()?;
        let mut current = BTreeMap::new();
        for (task, public) in Store::document_bindings(doc)? {
            let full = versions
                .get(&version_key(&task, &public)?)
                .ok_or("private task confirmation unavailable")?;
            current.insert(task, full.clone());
        }
        Store::from_confirmations(current)?.validate_document(doc)
    }

    fn confirmation_versions(&self) -> Result<Versions, String> {
        let values: Versions = super::private_tasks::read_private(
            &self
                .path
                .with_extension("private-task-confirmations-v1.json"),
        )?
        .unwrap_or_default();
        // The same bounds and draft validation apply to restored custody.
        Store::from_confirmations(values.clone())?;
        Ok(values)
    }

    pub(super) fn task_confirmations(
        &self,
        snapshot: &WorkspaceTaskSnapshot,
    ) -> Result<Store, String> {
        let versions = self.confirmation_versions()?;
        let mut current = BTreeMap::new();
        for (task, control) in &snapshot.controls {
            if let Some(public) = &control.confirmation {
                let key = version_key(task, public)?;
                let full = versions
                    .get(&key)
                    .ok_or("private task confirmation unavailable")?;
                if Store::public_confirmation(full) != *public {
                    return Err("private task confirmation binding changed".into());
                }
                current.insert(task.clone(), full.clone());
            }
        }
        Store::from_confirmations(current)
    }

    /// Owner-only policy input. Never send a hydrated snapshot over room sync.
    pub(super) fn task_policy_snapshot(&self) -> Result<WorkspaceTaskSnapshot, String> {
        let snapshot = workspace_task::snapshot(
            self.peer
                .admitted_room()
                .ok_or("private task room absent")?,
        )?;
        self.task_confirmations(&snapshot)?.hydrate(snapshot)
    }

    /// Remove superseded bodies only after the public room commit is durable.
    pub(super) fn compact_task_confirmations(&self) -> Result<(), String> {
        let snapshot = workspace_task::snapshot(
            self.peer
                .admitted_room()
                .ok_or("private task room absent")?,
        )?;
        let store = self.task_confirmations(&snapshot)?;
        let mut versions = Versions::new();
        for (task, full) in store.confirmations() {
            versions.insert(
                version_key(task, &Store::public_confirmation(full))?,
                full.clone(),
            );
        }
        let bytes = serde_json::to_vec(&versions).map_err(|e| e.to_string())?;
        tilo_files::native::atomic::write_private(
            &self
                .path
                .with_extension("private-task-confirmations-v1.json"),
            &bytes,
        )
        .map_err(|_| "private task confirmation compaction failed".into())
    }

    /// Write ahead before committing a scrubbed candidate. Older versions remain
    /// private and are never selected unless canonical state names them exactly.
    pub(super) fn persist_task_confirmations(&self, store: &Store) -> Result<(), String> {
        let mut versions = self.confirmation_versions()?;
        for (task, full) in store.confirmations() {
            let key = version_key(task, &Store::public_confirmation(full))?;
            if let Some(old) = versions.get(&key) {
                if old != full {
                    return Err("private task confirmation version changed".into());
                }
            } else {
                versions.insert(key, full.clone());
            }
        }
        Store::from_confirmations(versions.clone())?;
        let bytes = serde_json::to_vec(&versions).map_err(|e| e.to_string())?;
        if bytes.len() > 72 * 1024 * 1024 {
            return Err("private task confirmation custody exceeds capacity".into());
        }
        tilo_files::native::atomic::write_private(
            &self
                .path
                .with_extension("private-task-confirmations-v1.json"),
            &bytes,
        )
        .map_err(|_| "private task confirmation persistence failed".into())
    }
}
