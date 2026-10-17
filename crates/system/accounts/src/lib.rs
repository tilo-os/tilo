//! The shared **online-accounts** model + store, used by every Tilo app that consumes
//! external accounts (Mail, Calendar, and the Settings hub).
//!
//! # Why one crate
//!
//! Before this crate, Mail, Calendar and Settings each re-declared their own
//! `AccountStore`/`Account` structs and only agreed on a JSON path — and the shapes didn't
//! even match (an account added in Settings' calendar store was invisible to Calendar's).
//! This crate replaces those divergent copies with one canonical model, one persistence
//! location, and one encrypted vault for credentials that never belong in the plaintext store.
//!
//! # The pieces
//!
//! - [`Account`] — the type-agnostic envelope. `config` is opaque type-specific JSON,
//!   (de)serialised by the owning type. **No secret fields live here.**
//! - [`AccountStore`] — the canonical multi-account store, one pretty-JSON document at
//!   `$XDG_CONFIG_HOME/tilo/accounts/store.json`, holding every account plus the per-type
//!   active selection.
//! - [`SecretVault`] — the encrypted-at-rest credential store keyed by [`AccountId`] (see
//!   [`vault`]). Credentials go here, never into [`AccountStore`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Account type ids of the cloud storage accounts Settings connects and the
/// storage daemon mounts. One definition, so the two sides cannot disagree.
pub mod cloud_types {
    use serde::{Deserialize, Serialize};

    /// Whether Files keeps a remote account mounted or copies it once.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub enum FilesMode {
        Sync,
        Import,
    }

    /// Non-secret Files mode metadata key.
    pub const FILES_MODE_KEY: &str = "files_mode";
    /// Optional identity of the linked email account.
    pub const MAIL_ACCOUNT_ID_KEY: &str = "mail_account_id";

    /// Legacy accounts sync. Unknown explicit modes fail closed.
    pub fn files_mode(account: &crate::Account) -> Option<FilesMode> {
        match account.config.get(FILES_MODE_KEY) {
            None => Some(FilesMode::Sync),
            Some(value) => serde_json::from_value(value.clone()).ok(),
        }
    }

    /// Update Files mode without changing other connection metadata.
    pub fn set_files_mode(config: &mut serde_json::Value, mode: FilesMode) {
        if !config.is_object() {
            *config = serde_json::json!({});
        }
        if let Some(config) = config.as_object_mut() {
            config.insert(FILES_MODE_KEY.into(), serde_json::json!(mode));
        }
    }

    /// Google Drive.
    pub const GOOGLE_DRIVE: &str = "google-drive";
    /// `OneDrive` (personal or work/school) and `SharePoint` libraries.
    pub const ONEDRIVE: &str = "onedrive";
    /// Dropbox.
    pub const DROPBOX: &str = "dropbox";
    /// `iCloud` Drive.
    pub const ICLOUD_DRIVE: &str = "icloud-drive";
    /// Nextcloud (files, calendars, contacts).
    pub const NEXTCLOUD: &str = "nextcloud";
    /// A plain `WebDAV` server.
    pub const WEBDAV: &str = "webdav";

    /// The key of an account's resolved mount configuration in its
    /// non-secret `config`: the provider crate's own serde configuration.
    pub const MOUNT_CONFIG_KEY: &str = "mount";
}

pub mod email;
pub mod handoff;
pub mod thunderbird;
pub use handoff::{
    CommandRefresher, CredentialRefresher, HandoffCredential, HandoffCredentialSource,
    HandoffStore, RefreshError,
};
pub mod vault;
pub use vault::{Secret, SecretCustody, SecretVault};
pub mod secrets;
pub use secrets::{
    AccessDecision, AccessRequest, BrokerError, CredentialBroker, CredentialGrant, CredentialOwner,
    CredentialProposal, CredentialScope, GeneratedPasswordPolicy, GrantLifetime, SecretId,
    SecretItem, SecretItemKind, SecretStore,
};
pub mod service_credentials;
#[cfg(test)]
mod service_credentials_tests;
pub use service_credentials::{
    AccountCredentials, CredentialRef, ServiceAuthorization, ServiceCredentials,
};
#[cfg(feature = "person")]
pub mod person;
pub mod status;
pub use status::{
    AccountSyncHealth, AccountSyncReporter, AccountSyncStatus, AccountSyncStatusStore,
};

/// The kind of an account — `"email"`, `"caldav"`, … Owned by the type that contributes it;
/// the store treats it as an opaque tag it can group by.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountTypeId(pub String);

impl AccountTypeId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AccountTypeId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}
impl From<String> for AccountTypeId {
    fn from(s: String) -> Self {
        Self(s)
    }
}
impl std::fmt::Display for AccountTypeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable identifier for one configured account. Natural keys (an email address, a `CalDAV`
/// login) are fine, but an explicit id means a rename doesn't reshuffle the store or strand
/// the account's vault secret.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(pub String);

impl AccountId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AccountId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}
impl From<String> for AccountId {
    fn from(s: String) -> Self {
        Self(s)
    }
}
impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One configured external account. `config` is opaque to the store — the owning type
/// serialises its connection settings into it — and **carries no secrets**; credentials
/// live only in the [`SecretVault`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub type_id: AccountTypeId,
    /// Human label for the account ("Work — Fastmail"). Usually mirrors a field inside
    /// `config`, hoisted here so the store can list accounts without understanding the type.
    #[serde(default)]
    pub display_name: String,
    /// Type-specific connection settings. Opaque JSON; no secret fields.
    #[serde(default)]
    pub config: serde_json::Value,
    /// Whether the account is active (vs. temporarily disabled by the user).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// A short accent hint used to tint the account's avatar so multiple accounts read as
    /// visually distinct.
    #[serde(default)]
    pub tint: u8,
    /// The collaborator that owns this account, when it was contributed in a shared workspace.
    /// `None` for the local user's own accounts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

impl Account {
    /// A minimal account of `type_id` with an empty config. Callers fill `config` /
    /// `display_name` / `tint` before upserting.
    pub fn new(id: impl Into<AccountId>, type_id: impl Into<AccountTypeId>) -> Self {
        Self {
            id: id.into(),
            type_id: type_id.into(),
            display_name: String::new(),
            config: serde_json::Value::Null,
            enabled: true,
            tint: 0,
            owner: None,
        }
    }

    /// Decode the opaque `config` into a concrete type. `None` when the shape does not match (a
    /// store written by an older/newer type version).
    pub fn config_as<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_value(self.config.clone()).ok()
    }

    /// Set `config` from a concrete type. Serialisation failures are dropped (leaving config
    /// untouched) — the config is always plain data.
    pub fn set_config<T: Serialize>(&mut self, config: &T) {
        if let Ok(v) = serde_json::to_value(config) {
            self.config = v;
        }
    }
}

fn default_true() -> bool {
    true
}

/// The canonical multi-account store: every configured account of every type, plus which
/// account is active per type. Persisted as one pretty-JSON document at [`store_path`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStore {
    #[serde(default)]
    accounts: Vec<Account>,
    /// The active account id per type. A type with no entry uses its first account.
    #[serde(default)]
    active: BTreeMap<AccountTypeId, AccountId>,
    /// Whether this file is the projection of the person's accounts record
    /// rather than accounts configured on this device alone, which the
    /// record has yet to take in.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    projected: bool,
}

impl AccountStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the canonical store from the default location, or an empty store when it is missing
    /// / unreadable. Never an error to the caller.
    pub fn load() -> Self {
        match config_root() {
            Some(root) => Self::load_in(&root),
            None => Self::new(),
        }
    }

    /// Load the store under an explicit config root (`root/accounts/store.json`).
    pub fn load_in(config_root: &Path) -> Self {
        std::fs::read_to_string(store_path_in(config_root))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist the store to the default location.
    ///
    /// # Errors
    ///
    /// Returns an error if no config directory is available, or
    /// [`AccountStore::save_in`] fails.
    pub fn save(&self) -> std::io::Result<()> {
        let root = config_root().ok_or_else(|| std::io::Error::other("no config dir"))?;
        self.save_in(&root)
    }

    /// Persist the store under an explicit config root.
    ///
    /// # Errors
    ///
    /// Returns an error if the store's directory can't be created or the
    /// file can't be written.
    pub fn save_in(&self, config_root: &Path) -> std::io::Result<()> {
        let path = store_path_in(config_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        tilo_files::native::atomic::write(&path, json.as_bytes())
    }

    /// Every account in the store, in insertion order.
    pub fn all(&self) -> &[Account] {
        &self.accounts
    }

    /// Every account of `type_id`, in insertion order.
    pub fn list(&self, type_id: &AccountTypeId) -> Vec<&Account> {
        self.accounts
            .iter()
            .filter(|a| &a.type_id == type_id)
            .collect()
    }

    /// Look up an account by id, regardless of type.
    pub fn get(&self, id: &AccountId) -> Option<&Account> {
        self.accounts.iter().find(|a| &a.id == id)
    }

    /// Add or replace (by id) an account, preserving insertion position on replace. If this is
    /// the type's first account it becomes active.
    pub fn upsert(&mut self, account: Account) {
        let type_id = account.type_id.clone();
        let is_first_of_type = !self.accounts.iter().any(|a| a.type_id == type_id);
        if let Some(slot) = self.accounts.iter_mut().find(|a| a.id == account.id) {
            *slot = account;
        } else {
            let id = account.id.clone();
            self.accounts.push(account);
            if is_first_of_type {
                self.active.insert(type_id, id);
            }
        }
    }

    /// Remove an account by id. Clears the per-type active pointer if it named the removed
    /// account, falling back to the type's first remaining one.
    pub fn remove(&mut self, id: &AccountId) -> bool {
        let Some(pos) = self.accounts.iter().position(|a| &a.id == id) else {
            return false;
        };
        let removed = self.accounts.remove(pos);
        if self.active.get(&removed.type_id) == Some(id) {
            match self.list(&removed.type_id).first() {
                Some(next) => {
                    self.active.insert(removed.type_id.clone(), next.id.clone());
                }
                None => {
                    self.active.remove(&removed.type_id);
                }
            }
        }
        true
    }

    /// The active account for a type: the explicitly-selected one, else the type's first
    /// account, else `None`.
    pub fn active(&self, type_id: &AccountTypeId) -> Option<&Account> {
        if let Some(id) = self.active.get(type_id)
            && let Some(a) = self
                .accounts
                .iter()
                .find(|a| &a.id == id && &a.type_id == type_id)
        {
            return Some(a);
        }
        self.list(type_id).into_iter().next()
    }

    /// Select the active account for a type. No-op if `id` is not an account of `type_id`.
    pub fn set_active(&mut self, type_id: &AccountTypeId, id: &AccountId) {
        if self
            .accounts
            .iter()
            .any(|a| &a.id == id && &a.type_id == type_id)
        {
            self.active.insert(type_id.clone(), id.clone());
        }
    }

    /// The active account id for a type, if one is recorded.
    pub fn active_id(&self, type_id: &AccountTypeId) -> Option<&AccountId> {
        self.active(type_id).map(|a| &a.id)
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Whether this store is the projection of the person's accounts
    /// record: what the record holds, not accounts it has yet to take in.
    pub fn is_projection(&self) -> bool {
        self.projected
    }

    /// This store, marked as the projection of the person's accounts.
    #[must_use]
    pub fn as_projection(mut self) -> Self {
        self.projected = true;
        self
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }
}

/// The per-user account config root: `$XDG_CONFIG_HOME/tilo` or `~/.config/tilo`.
/// Like Settings, ignore empty or relative XDG paths.
pub fn config_root() -> Option<PathBuf> {
    Some(tilo_paths::xdg::config_home()?.join("tilo"))
}

/// The accounts directory under a config root (`root/accounts`).
pub fn accounts_dir_in(config_root: &Path) -> PathBuf {
    config_root.join("accounts")
}

/// The canonical store file under a config root (`root/accounts/store.json`).
pub fn store_path_in(config_root: &Path) -> PathBuf {
    accounts_dir_in(config_root).join("store.json")
}

/// The vault directory under a config root (`root/accounts/vault`).
pub fn vault_dir_in(config_root: &Path) -> PathBuf {
    accounts_dir_in(config_root).join("vault")
}

/// The canonical store file at the default config root, if resolvable.
pub fn store_path() -> Option<PathBuf> {
    config_root().map(|r| store_path_in(&r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct DemoConfig {
        host: String,
        port: u16,
    }

    fn email(id: &str, host: &str) -> Account {
        let mut a = Account::new(id, "email");
        a.display_name = format!("{id} inbox");
        a.set_config(&DemoConfig {
            host: host.into(),
            port: 993,
        });
        a
    }

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("tilo-accounts-store-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn files_modes_preserve_legacy_sync_and_reject_unknown_metadata() {
        use cloud_types::{FilesMode, files_mode, set_files_mode};
        let mut account = Account::new("drive", cloud_types::GOOGLE_DRIVE);
        assert_eq!(files_mode(&account), Some(FilesMode::Sync));
        account.config = serde_json::json!({"provider": "google-drive", "mail_account_id": "mail"});
        set_files_mode(&mut account.config, FilesMode::Import);
        assert_eq!(files_mode(&account), Some(FilesMode::Import));
        assert_eq!(account.config["files_mode"], "import");
        assert_eq!(account.config["mail_account_id"], "mail");
        assert!(account.enabled, "import does not disable the account");
        set_files_mode(&mut account.config, FilesMode::Sync);
        assert_eq!(account.config["files_mode"], "sync");
        for invalid in [
            serde_json::json!("unknown"),
            serde_json::json!(null),
            serde_json::json!(true),
        ] {
            account.config["files_mode"] = invalid;
            assert_eq!(files_mode(&account), None);
        }
    }

    #[test]
    fn store_upserts_by_id_and_tracks_the_active_account_per_type() {
        // Phase `upsert_replaces_by_id_and_lists_by_type`.
        {
            let mut store = AccountStore::new();
            store.upsert(email("a@x.example", "imap.x"));
            store.upsert(email("b@y.example", "imap.y"));
            let mut cal = Account::new("cal-1", "caldav");
            cal.display_name = "Work".into();
            store.upsert(cal);

            assert_eq!(store.list(&"email".into()).len(), 2);
            assert_eq!(store.list(&"caldav".into()).len(), 1);

            // Upsert by id replaces in place, does not append.
            store.upsert(email("a@x.example", "imap.new"));
            assert_eq!(store.list(&"email".into()).len(), 2);
            assert_eq!(
                store
                    .get(&"a@x.example".into())
                    .unwrap()
                    .config_as::<DemoConfig>()
                    .unwrap()
                    .host,
                "imap.new"
            );
        }

        // Phase `active_defaults_to_first_and_follows_selection_and_removal`.
        {
            let mut store = AccountStore::new();
            store.upsert(email("a@x.example", "imap.x"));
            store.upsert(email("b@y.example", "imap.y"));
            let email_ty: AccountTypeId = "email".into();

            // First account of a type is active by default.
            assert_eq!(store.active(&email_ty).unwrap().id, "a@x.example".into());
            // Explicit selection wins.
            store.set_active(&email_ty, &"b@y.example".into());
            assert_eq!(store.active(&email_ty).unwrap().id, "b@y.example".into());
            // Removing the active account falls back to a remaining one.
            assert!(store.remove(&"b@y.example".into()));
            assert_eq!(store.active(&email_ty).unwrap().id, "a@x.example".into());
        }
    }

    #[test]
    fn store_round_trips_through_disk_without_carrying_a_secret() {
        // Phase `store_round_trips_through_disk`.
        {
            let root = temp_root();
            let mut store = AccountStore::new();
            store.upsert(email("a@x.example", "imap.x"));
            store.set_active(&"email".into(), &"a@x.example".into());
            store.save_in(&root).expect("save");

            let back = AccountStore::load_in(&root);
            assert_eq!(back, store);
            // Pretty JSON on disk.
            let raw = std::fs::read_to_string(store_path_in(&root)).unwrap();
            assert!(raw.contains('\n'), "the store is written as pretty JSON");
            std::fs::remove_dir_all(&root).ok();
        }

        // Phase `account_config_carries_no_secret_by_construction`.
        {
            // The envelope has no secret field; a type's config is plain data, so persisting a store
            // cannot leak a credential.
            let root = temp_root();
            let mut store = AccountStore::new();
            store.upsert(email("a@x.example", "imap.x"));
            store.save_in(&root).unwrap();
            let raw = std::fs::read_to_string(store_path_in(&root)).unwrap();
            assert!(!raw.to_lowercase().contains("password"));
            std::fs::remove_dir_all(&root).ok();
        }
    }
}
