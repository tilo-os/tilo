//! Runtime sync health reported by account consumers.
//!
//! Connection settings and secrets have different writers and lifecycles from
//! operational health, so this is a separate atomic document. Mail, Calendar
//! and file sync workers report transitions here; Settings only reads them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::AccountId;

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

    pub fn save(&self) -> std::io::Result<()> {
        let root = crate::config_root().ok_or_else(|| std::io::Error::other("no config dir"))?;
        self.save_in(&root)
    }

    pub fn save_in(&self, config_root: &Path) -> std::io::Result<()> {
        let path = path_in(config_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        atomic_file::write(&path, &json)
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
    fn status_round_trips_and_messages_are_bounded_one_line() {
        let root =
            std::env::temp_dir().join(format!("qos-account-status-{}", uuid::Uuid::new_v4()));
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
}
