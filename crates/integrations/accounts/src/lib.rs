//! The online-account model every Tilo app that consumes external accounts shares (Mail,
//! Calendar, Files and the Settings hub), and one encrypted vault for their credentials.
//!
//! - [`Account`] — the type-agnostic envelope. `config` is opaque type-specific JSON,
//!   (de)serialised by the owning type. **No secret fields live here.** The accounts
//!   themselves are the workspace's record ([`person::AccountList`]); this is its shape.
//! - [`SecretVault`] — the encrypted-at-rest credential store keyed by [`AccountId`] (see
//!   [`vault`]). Credentials go here, never into the accounts record.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Account type ids of the cloud storage accounts Settings connects and the
/// storage daemon mounts. One definition, so the two sides cannot disagree.
pub mod cloud_types {
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub enum FilesMode {
        Sync,
        Import,
    }

    pub const FILES_MODE_KEY: &str = "files_mode";
    pub const MAIL_ACCOUNT_ID_KEY: &str = "mail_account_id";

    /// Legacy accounts sync. Unknown explicit modes fail closed.
    pub fn files_mode(account: &crate::Account) -> Option<FilesMode> {
        mode_of(&account.config)
    }

    /// The Files mode `config` names, when it is one this build knows.
    pub fn mode_of(config: &serde_json::Value) -> Option<FilesMode> {
        match config.get(FILES_MODE_KEY) {
            None => Some(FilesMode::Sync),
            Some(value) => serde_json::from_value(value.clone()).ok(),
        }
    }

    pub fn set_files_mode(config: &mut serde_json::Value, mode: FilesMode) {
        if !config.is_object() {
            *config = serde_json::json!({});
        }
        if let Some(config) = config.as_object_mut() {
            config.insert(FILES_MODE_KEY.into(), serde_json::json!(mode));
        }
    }

    pub const GOOGLE_DRIVE: &str = "google-drive";
    pub const ONEDRIVE: &str = "onedrive";
    pub const DROPBOX: &str = "dropbox";
    pub const ICLOUD_DRIVE: &str = "icloud-drive";
    pub const NEXTCLOUD: &str = "nextcloud";
    pub const WEBDAV: &str = "webdav";

    /// The key of an account's resolved mount configuration in its
    /// non-secret `config`: the provider crate's own serde configuration.
    pub const MOUNT_CONFIG_KEY: &str = "mount";
}

pub mod email;
pub mod thunderbird;
pub mod vault;
pub use vault::{Saved, Secret, SecretVault, VaultBackend};
pub mod secrets;
#[cfg(any(unix, windows))]
pub mod sign_ins;
pub mod typed_for;
pub use secrets::{
    AccessDecision, AccessRequest, BrokerError, CredentialBroker, CredentialGrant, CredentialOwner,
    CredentialProposal, CredentialScope, GeneratedPasswordPolicy, GrantLifetime, SecretId,
    SecretItem, SecretItemKind, SecretStore, is_entry_id,
};
pub mod service_credentials;
#[cfg(test)]
mod service_credentials_tests;
pub use service_credentials::{
    AccountCredentials, CredentialRef, MODEL_SERVICE, ServiceAuthorization, ServiceCredentials,
    provider_api_key,
};
#[cfg(feature = "person")]
pub mod calendar;
#[cfg(feature = "person")]
pub mod person;
pub mod status;
pub mod vault_items;
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
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// A short accent hint used to tint the account's avatar so multiple accounts read as
    /// visually distinct.
    #[serde(default)]
    pub tint: u8,
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

/// Whether an account's sign-in may be sent to `endpoint`: over HTTPS, or
/// over plain HTTP only to this machine's loopback (`localhost`, `127.0.0.1`,
/// `[::1]`), which never leaves it. The one rule for every account server:
/// signing in, discovering calendars, showing an account and mounting its
/// files all ask it.
pub use cloud_source::is_secure_endpoint;

/// The per-user account config root: `$XDG_CONFIG_HOME/tilo` or `~/.config/tilo`.
/// Like Settings, ignore empty or relative XDG paths.
pub fn config_root() -> Option<PathBuf> {
    Some(tilo_paths::xdg::config_home()?.join("tilo"))
}

pub fn accounts_dir_in(config_root: &Path) -> PathBuf {
    config_root.join("accounts")
}

pub fn vault_dir_in(config_root: &Path) -> PathBuf {
    accounts_dir_in(config_root).join("vault")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sign_in_goes_over_https_or_to_this_machine_alone() {
        for allowed in [
            "https://cloud.example/remote.php/dav",
            "http://127.0.0.1:8080/",
            "http://127.1.2.3/",
            "http://[::1]:8080/",
            "http://localhost:8080/",
        ] {
            assert!(super::is_secure_endpoint(allowed), "{allowed}");
        }
        for refused in [
            "http://cloud.example/",
            "http://10.0.0.1/",
            "http://192.168.1.2:8080/",
            "http://localhost.example/",
            "http://127.0.0.1.example/",
            "ftp://127.0.0.1/",
            "not a server",
        ] {
            assert!(!super::is_secure_endpoint(refused), "{refused}");
        }
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
}
