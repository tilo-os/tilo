//! Runtime sync health reported by account consumers.
//!
//! Connection settings and secrets have different writers and lifecycles from
//! operational health, so this is a separate atomic document. Mail, Calendar
//! and file sync workers report transitions here; Settings only reads them.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::AccountId;

/// The single write path for operational account health. Consumers report
/// lifecycle transitions here instead of rebuilding timestamps (and commonly
/// losing the last successful sync) themselves.
#[derive(Clone, Debug)]
pub struct AccountSyncReporter {
    id: AccountId,
    config_root: Option<PathBuf>,
}

impl AccountSyncReporter {
    pub fn new(id: impl Into<AccountId>) -> Self {
        Self {
            id: id.into(),
            config_root: crate::config_root(),
        }
    }

    pub fn in_root(id: impl Into<AccountId>, config_root: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            config_root: Some(config_root.into()),
        }
    }

    /// # Errors
    ///
    /// Returns an error if no config directory is available, the status
    /// lock can't be acquired, or the updated status can't be saved.
    pub fn started(&self) -> std::io::Result<()> {
        self.transition(AccountSyncHealth::Syncing, false, Some("Syncing"))
    }

    /// # Errors
    ///
    /// See [`AccountSyncReporter::started`].
    pub fn succeeded(&self) -> std::io::Result<()> {
        self.transition(AccountSyncHealth::Healthy, true, Some("Up to date"))
    }

    /// # Errors
    ///
    /// See [`AccountSyncReporter::started`].
    pub fn offline(&self) -> std::io::Result<()> {
        self.transition(
            AccountSyncHealth::Offline,
            false,
            Some("Waiting for a connection"),
        )
    }

    /// # Errors
    ///
    /// See [`AccountSyncReporter::started`].
    pub fn needs_authentication(&self) -> std::io::Result<()> {
        self.transition(
            AccountSyncHealth::NeedsAuthentication,
            false,
            Some("Sign-in required"),
        )
    }

    /// Raw protocol/transport errors deliberately are not accepted here: they
    /// commonly contain credential-bearing URLs or response bodies.
    ///
    /// # Errors
    ///
    /// See [`AccountSyncReporter::started`].
    pub fn failed(&self) -> std::io::Result<()> {
        self.transition(AccountSyncHealth::Error, false, Some("Sync failed"))
    }

    fn transition(
        &self,
        health: AccountSyncHealth,
        success: bool,
        message: Option<&str>,
    ) -> std::io::Result<()> {
        let root = self
            .config_root
            .as_deref()
            .ok_or_else(|| std::io::Error::other("no config dir"))?;
        let _lock = StatusLock::acquire(root)?;
        let mut store = AccountSyncStatusStore::load_in(root);
        let previous_success = store
            .get(&self.id)
            .and_then(|status| status.last_success_unix);
        let now = tilo_clock::unix_now_i64();
        store.report(
            &self.id,
            AccountSyncStatus::sanitized(
                health,
                Some(now),
                success.then_some(now).or(previous_success),
                message,
            ),
        );
        store.save_in_unlocked(root)
    }

    /// # Errors
    ///
    /// Returns an error if no config directory is available, the status
    /// lock can't be acquired, or the updated store can't be saved.
    pub fn remove(&self) -> std::io::Result<()> {
        let root = self
            .config_root
            .as_deref()
            .ok_or_else(|| std::io::Error::other("no config dir"))?;
        let _lock = StatusLock::acquire(root)?;
        let mut store = AccountSyncStatusStore::load_in(root);
        store.remove(&self.id);
        store.save_in_unlocked(root)
    }
}

struct StatusLock {
    _lock: advisory_lock::FileLock,
}

impl StatusLock {
    fn acquire(config_root: &Path) -> std::io::Result<Self> {
        let path = crate::accounts_dir_in(config_root).join("sync-status.lock");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            // This file holds no content, only the lock itself; state that
            // explicitly rather than leaving `create`'s truncation implicit.
            .truncate(false);
        #[cfg(unix)]
        options.mode(0o600);
        Ok(Self {
            _lock: advisory_lock::FileLock::exclusive(options.open(path)?)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountSyncHealth {
    #[default]
    Never,
    Syncing,
    Healthy,
    Offline,
    NeedsAuthentication,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSyncStatus {
    pub health: AccountSyncHealth,
    #[serde(default)]
    pub last_attempt_unix: Option<i64>,
    #[serde(default)]
    pub last_success_unix: Option<i64>,
    /// Sanitized, bounded operational summary. It must never contain a
    /// credential or server response body.
    #[serde(default)]
    pub message: Option<String>,
}

impl AccountSyncStatus {
    pub fn sanitized(
        health: AccountSyncHealth,
        last_attempt_unix: Option<i64>,
        last_success_unix: Option<i64>,
        message: Option<&str>,
    ) -> Self {
        Self {
            health,
            last_attempt_unix,
            last_success_unix,
            message: message.map(sanitize_message),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSyncStatusStore {
    #[serde(default)]
    statuses: BTreeMap<String, AccountSyncStatus>,
}

impl AccountSyncStatusStore {
    pub fn load() -> Self {
        match crate::config_root() {
            Some(root) => Self::load_in(&root),
            None => Self::default(),
        }
    }

    pub fn load_in(config_root: &Path) -> Self {
        std::fs::read_to_string(path_in(config_root))
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    pub fn get(&self, id: &AccountId) -> Option<&AccountSyncStatus> {
        self.statuses.get(id.as_str())
    }

    pub fn report(&mut self, id: &AccountId, status: AccountSyncStatus) {
        self.statuses.insert(id.as_str().to_string(), status);
    }

    pub fn remove(&mut self, id: &AccountId) {
        self.statuses.remove(id.as_str());
    }

    /// # Errors
    ///
    /// Returns an error if no config directory is available, or
    /// [`AccountSyncStatusStore::save_in`] fails.
    pub fn save(&self) -> std::io::Result<()> {
        let root = crate::config_root().ok_or_else(|| std::io::Error::other("no config dir"))?;
        self.save_in(&root)
    }

    /// # Errors
    ///
    /// Returns an error if the status lock can't be acquired, `self` can't
    /// be serialised, or the file can't be written.
    pub fn save_in(&self, config_root: &Path) -> std::io::Result<()> {
        let _lock = StatusLock::acquire(config_root)?;
        self.save_in_unlocked(config_root)
    }

    fn save_in_unlocked(&self, config_root: &Path) -> std::io::Result<()> {
        let path = path_in(config_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        tilo_files::native::atomic::write(&path, &json)
    }
}

pub fn path_in(config_root: &Path) -> PathBuf {
    crate::accounts_dir_in(config_root).join("sync-status.json")
}

fn sanitize_message(message: &str) -> String {
    let one_line: String = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(240)
        .collect();
    one_line.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_status_persists_bounded_messages_last_success_and_concurrent_reports() {
        // Phase `status_round_trips_and_messages_are_bounded_one_line`.
        {
            let root =
                std::env::temp_dir().join(format!("tilo-account-status-{}", uuid::Uuid::new_v4()));
            let id = AccountId::from("mail@example.test");
            let mut store = AccountSyncStatusStore::default();
            store.report(
                &id,
                AccountSyncStatus::sanitized(
                    AccountSyncHealth::Error,
                    Some(20),
                    Some(10),
                    Some(&format!("login failed\n{}", "x".repeat(400))),
                ),
            );
            store.save_in(&root).unwrap();
            let loaded = AccountSyncStatusStore::load_in(&root);
            let message = loaded.get(&id).unwrap().message.as_deref().unwrap();
            assert!(!message.contains('\n'));
            assert!(message.len() <= 240);
            std::fs::remove_dir_all(root).ok();
        }

        // Phase `reporter_preserves_last_success_across_auth_failure`.
        {
            let root = std::env::temp_dir()
                .join(format!("tilo-account-reporter-{}", uuid::Uuid::new_v4()));
            let id = AccountId::from("calendar@example.test");
            let reporter = AccountSyncReporter::in_root(id.clone(), &root);
            reporter.started().unwrap();
            reporter.succeeded().unwrap();
            let successful = AccountSyncStatusStore::load_in(&root)
                .get(&id)
                .unwrap()
                .clone();
            reporter.needs_authentication().unwrap();
            let auth = AccountSyncStatusStore::load_in(&root)
                .get(&id)
                .unwrap()
                .clone();
            assert_eq!(auth.health, AccountSyncHealth::NeedsAuthentication);
            assert_eq!(auth.last_success_unix, successful.last_success_unix);
            assert_eq!(auth.message.as_deref(), Some("Sign-in required"));
            std::fs::remove_dir_all(root).ok();
        }

        // Phase `reporters_serialize_cross_thread_updates`.
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-account-reporter-lock-{}",
                uuid::Uuid::new_v4()
            ));
            let first = AccountId::from("first");
            let second = AccountId::from("second");
            let a = AccountSyncReporter::in_root(first.clone(), &root);
            let b = AccountSyncReporter::in_root(second.clone(), &root);
            let one = std::thread::spawn(move || a.succeeded());
            let two = std::thread::spawn(move || b.needs_authentication());
            one.join().unwrap().unwrap();
            two.join().unwrap().unwrap();
            let statuses = AccountSyncStatusStore::load_in(&root);
            assert_eq!(
                statuses.get(&first).unwrap().health,
                AccountSyncHealth::Healthy
            );
            assert_eq!(
                statuses.get(&second).unwrap().health,
                AccountSyncHealth::NeedsAuthentication
            );
            std::fs::remove_dir_all(root).ok();
        }
    }
}
