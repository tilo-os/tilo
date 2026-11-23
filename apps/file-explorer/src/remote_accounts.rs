//! The external accounts Files shows, as the workspace's accounts record holds them
//! ([`accounts::person::AccountList`], the record Settings keeps and every app follows): where
//! each one mounts, and what to say when it cannot. Files reads no account file of its own; the
//! system's cloud service, which keeps each account's folder in sync and browses one that is only
//! imported from, is what reaches the server with the account's secret.

use std::path::{Path, PathBuf};

use accounts::cloud_types::{FilesMode, files_mode};
use accounts::{Account, AccountId, AccountTypeId};
use serde_json::Value;

pub const WEBDAV_TYPE: &str = "webdav";
pub const NEXTCLOUD_TYPE: &str = "nextcloud";
pub const GOOGLE_DRIVE_TYPE: &str = "google-drive";
pub const ONEDRIVE_TYPE: &str = "onedrive";
pub const DROPBOX_TYPE: &str = "dropbox";
pub const ICLOUD_DRIVE_TYPE: &str = "icloud-drive";
pub const MAX_REMOTE_ACCOUNTS: usize = 64;
const MAX_LABEL_CHARS: usize = 120;
const MAX_HOST_CHARS: usize = 253;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAccount {
    pub id: AccountId,
    pub kind: RemoteAccountKind,
    pub label: String,
    pub host: String,
    pub health: RemoteAccountHealth,
    pub files_mode: FilesMode,
    pub mount: Option<PathBuf>,
}

impl RemoteAccount {
    pub fn is_import(&self) -> bool {
        self.files_mode == FilesMode::Import
    }

    pub fn badge(&self) -> Option<String> {
        if self.is_import()
            && matches!(
                self.health,
                RemoteAccountHealth::Healthy
                    | RemoteAccountHealth::NeverChecked
                    | RemoteAccountHealth::Syncing
            )
        {
            Some(crate::i18n::t("files-account-import-only"))
        } else {
            self.health.badge().map(str::to_owned)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAccountKind {
    WebDav,
    Nextcloud,
    GoogleDrive,
    OneDrive,
    Dropbox,
    ICloudDrive,
}

impl RemoteAccountKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::WebDav => "WebDAV",
            Self::Nextcloud => "Nextcloud",
            Self::GoogleDrive => "Google Drive",
            Self::OneDrive => "OneDrive",
            Self::Dropbox => "Dropbox",
            Self::ICloudDrive => "iCloud Drive",
        }
    }

    /// A fixed public service name, not a configured credential endpoint.
    fn service_host(self) -> Option<&'static str> {
        match self {
            Self::WebDav | Self::Nextcloud => None,
            Self::GoogleDrive => Some("drive.google.com"),
            Self::OneDrive => Some("onedrive.live.com"),
            Self::Dropbox => Some("www.dropbox.com"),
            Self::ICloudDrive => Some("www.icloud.com"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAccountHealth {
    Disabled,
    NeverChecked,
    /// The person just signed in, and the folder has not synced yet.
    SignedIn,
    Syncing,
    Healthy,
    Offline,
    NeedsAuthentication,
    Error,
}

impl RemoteAccountHealth {
    pub fn label(self) -> &'static str {
        match self {
            Self::Disabled => "Disabled",
            Self::NeverChecked => "Not checked yet",
            Self::SignedIn => "Signed in",
            Self::Syncing => "Syncing",
            Self::Healthy => "Up to date",
            Self::Offline => "Offline",
            Self::NeedsAuthentication => "Sign-in required",
            Self::Error => "Needs attention",
        }
    }

    pub fn unavailable_reason(self) -> &'static str {
        match self {
            Self::Disabled => "This account is disabled in Settings",
            Self::Offline => {
                "Offline: files read before stay available and changes upload when the connection returns"
            }
            Self::NeedsAuthentication => "Sign in again in Settings to keep this folder in sync",
            Self::Error => "This account needs attention in Settings",
            Self::NeverChecked => "The account has not been checked yet",
            Self::SignedIn => "Signed in: the folder fills in as it syncs",
            Self::Syncing => "This folder is syncing",
            Self::Healthy => "This folder is up to date",
        }
    }

    pub fn badge(self) -> Option<&'static str> {
        match self {
            Self::Healthy => None,
            Self::Disabled => Some("Disabled"),
            Self::Offline => Some("Offline"),
            Self::NeedsAuthentication => Some("Sign in"),
            Self::Error => Some("Attention"),
            Self::NeverChecked | Self::SignedIn | Self::Syncing => Some("Syncing"),
        }
    }
}

/// The file accounts the workspace's accounts record holds, each with the folder it is kept in
/// under `home` (none without one).
pub fn from_record(
    record: &accounts::person::AccountList,
    home: Option<&Path>,
) -> Vec<RemoteAccount> {
    from_accounts(&record.all(), home)
}

fn from_accounts(all: &[Account], home: Option<&Path>) -> Vec<RemoteAccount> {
    let mut projected: Vec<RemoteAccount> = all
        .iter()
        .filter_map(from_account)
        .take(MAX_REMOTE_ACCOUNTS)
        .collect();
    if let Some(home) = home {
        assign_mounts(&mut projected, all, &home.join(MOUNTS_DIR));
    }
    projected
}

/// The accounts the window last read from the record, for what runs away from it: an import's
/// destination checked as it is admitted, and as it runs.
static FOLLOWED: std::sync::Mutex<Vec<RemoteAccount>> = std::sync::Mutex::new(Vec::new());

/// The accounts the window last read from the workspace's accounts record.
pub fn followed() -> Vec<RemoteAccount> {
    FOLLOWED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

pub(crate) fn keep_followed(accounts: &[RemoteAccount]) {
    accounts.clone_into(
        &mut FOLLOWED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
}

pub const MOUNTS_DIR: &str = "Remote";

/// Name each account's folder exactly as the cloud service does: every
/// sync file account's label (enabled or not), unique in account-id order.
/// Import accounts never reserve a folder name or receive a mount.
fn assign_mounts(projected: &mut [RemoteAccount], all: &[Account], root: &Path) {
    let labels: Vec<(&str, &str)> = all
        .iter()
        .filter(|account| files_mode(account) == Some(FilesMode::Sync))
        .filter(|account| is_file_account(account))
        .map(|account| (account.id.as_str(), account.display_name.as_str()))
        .collect();
    let names = cloud_source::tree::mount_names(&labels);
    for account in projected.iter_mut() {
        if account.is_import() {
            account.mount = None;
            continue;
        }
        account.mount = names
            .iter()
            .find(|(id, _)| id == account.id.as_str())
            .map(|(_, name)| root.join(name));
    }
}

fn is_file_account(account: &Account) -> bool {
    if matches!(
        account.type_id.as_str(),
        GOOGLE_DRIVE_TYPE | ONEDRIVE_TYPE | DROPBOX_TYPE | ICLOUD_DRIVE_TYPE
    ) {
        return true;
    }
    matches!(account.type_id.as_str(), WEBDAV_TYPE | NEXTCLOUD_TYPE)
        && (account.type_id.as_str() != NEXTCLOUD_TYPE
            || account
                .config
                .get("use_files")
                .and_then(Value::as_bool)
                .unwrap_or(true))
        && account
            .config
            .get("server_url")
            .and_then(Value::as_str)
            .is_some()
}

fn from_account(account: &Account) -> Option<RemoteAccount> {
    let files_mode = files_mode(account)?;
    let kind = match account.type_id.as_str() {
        WEBDAV_TYPE => RemoteAccountKind::WebDav,
        NEXTCLOUD_TYPE => RemoteAccountKind::Nextcloud,
        GOOGLE_DRIVE_TYPE => RemoteAccountKind::GoogleDrive,
        ONEDRIVE_TYPE => RemoteAccountKind::OneDrive,
        DROPBOX_TYPE => RemoteAccountKind::Dropbox,
        ICLOUD_DRIVE_TYPE => RemoteAccountKind::ICloudDrive,
        _ => return None,
    };
    if kind == RemoteAccountKind::Nextcloud
        && !account
            .config
            .get("use_files")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    {
        return None;
    }
    let host = match kind.service_host() {
        Some(host) => host.to_owned(),
        None => account
            .config
            .get("server_url")
            .and_then(Value::as_str)
            .and_then(server_host)?,
    };
    let label = bounded_label(if account.display_name.trim().is_empty() {
        kind.label()
    } else {
        account.display_name.as_str()
    });
    // How its folder's sync stands is the cloud service's to say: an enabled account reads as
    // not checked until the window hears from it (`files_app::cloud_status`).
    let health = if account.enabled {
        RemoteAccountHealth::NeverChecked
    } else {
        RemoteAccountHealth::Disabled
    };
    Some(RemoteAccount {
        id: account.id.clone(),
        kind,
        label,
        host,
        health,
        files_mode,
        mount: None,
    })
}

/// The host (and port) an account's server is named by, when it is one Files may show: one the
/// cloud service mounts ([`accounts::is_secure_endpoint`]: HTTPS, or plain HTTP to this
/// machine), naming no credential, query or fragment.
fn server_host(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix("https://").or_else(|| {
        raw.strip_prefix("http://")
            .filter(|_| accounts::is_secure_endpoint(raw))
    })?;
    if rest.is_empty() || rest.contains(['@', '?', '#', '\\']) {
        return None;
    }
    let authority = rest.split('/').next()?;
    if authority.is_empty() || authority.len() > MAX_HOST_CHARS {
        return None;
    }
    // A colon is allowed for a non-secret explicit port. Whitespace/control
    // characters are not a valid host and must never reach a text surface.
    if authority.chars().any(char::is_control) || authority.contains(char::is_whitespace) {
        return None;
    }
    Some(authority.to_ascii_lowercase())
}

fn bounded_label(label: &str) -> String {
    label
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_LABEL_CHARS)
        .collect()
}

pub fn resource_id(id: &AccountId) -> String {
    format!("files.remote-account:{}", hex::encode(id.as_str()))
}

pub fn type_id(kind: RemoteAccountKind) -> AccountTypeId {
    match kind {
        RemoteAccountKind::WebDav => WEBDAV_TYPE.into(),
        RemoteAccountKind::Nextcloud => NEXTCLOUD_TYPE.into(),
        RemoteAccountKind::GoogleDrive => GOOGLE_DRIVE_TYPE.into(),
        RemoteAccountKind::OneDrive => ONEDRIVE_TYPE.into(),
        RemoteAccountKind::Dropbox => DROPBOX_TYPE.into(),
        RemoteAccountKind::ICloudDrive => ICLOUD_DRIVE_TYPE.into(),
    }
}

pub fn account_of_resource(resource: &str) -> Option<String> {
    let hex_id = resource.strip_prefix("files.remote-account:")?;
    String::from_utf8(hex::decode(hex_id).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn remote_accounts_project_safely_from_the_accounts_record;

        phase import_accounts_are_visible_without_mounts_or_reserved_mount_names;
        {
            let home = tempfile::tempdir().unwrap();
            let mut store = Vec::new();
            for (id, mode) in [("a-import", "import"), ("b-sync", "sync")] {
                let mut account = Account::new(id, WEBDAV_TYPE);
                account.display_name = "Cloud".into();
                account.config = serde_json::json!({
                    "server_url": "https://files.example/dav",
                    "files_mode": mode,
                });
                store.push(account);
            }
            let loaded = from_accounts(&store, Some(home.path()));
            assert_eq!(loaded.len(), 2);
            let import = loaded.iter().find(|account| account.is_import()).unwrap();
            assert_eq!(import.id.as_str(), "a-import");
            assert_eq!(import.mount, None);
            let sync = loaded.iter().find(|account| !account.is_import()).unwrap();
            assert_eq!(sync.mount, Some(home.path().join("Remote/Cloud")));
            assert!(
                from_accounts(&store, None)
                    .iter()
                    .all(|account| account.mount.is_none())
            );
        }

        phase unknown_files_modes_are_not_projected;
        {
            let mut account = Account::new("cloud", GOOGLE_DRIVE_TYPE);
            account.config = serde_json::json!({"files_mode": "unknown"});
            assert!(from_account(&account).is_none());
            account.config = serde_json::json!({});
            let legacy = from_account(&account).unwrap();
            assert_eq!(legacy.files_mode, FilesMode::Sync);
            assert!(!legacy.is_import());
        }

        phase remote_links_use_cloud_daemon_names_when_oauth_provider_labels_collide;
        {
            for provider in ["google-drive", "onedrive", "dropbox", "icloud-drive"] {
                let home = tempfile::tempdir().unwrap();
                let mut store = Vec::new();
                let mut oauth = Account::new("a-cloud", provider);
                oauth.display_name = "Cloud".into();
                store.push(oauth);
                let mut dav = Account::new("z-dav", WEBDAV_TYPE);
                dav.display_name = "Cloud".into();
                dav.config = serde_json::json!({"server_url":"https://files.example/dav"});
                store.push(dav);
                let loaded = from_accounts(&store, Some(home.path()));
                let dav = loaded
                    .iter()
                    .find(|account| account.id.as_str() == "z-dav")
                    .unwrap();
                assert_eq!(
                    dav.mount,
                    Some(home.path().join("Remote/Cloud (2)")),
                    "{provider}"
                );
            }
        }

        phase accounts_keep_their_mount_folders_and_say_they_are_not_checked;
        {
            let home = tempfile::tempdir().unwrap();
            let mut store = Vec::new();
            for (id, label) in [("b", "Cloud"), ("a", "Cloud")] {
                let mut account = Account::new(id, WEBDAV_TYPE);
                account.display_name = label.into();
                account.config = serde_json::json!({"server_url": "https://files.example/dav"});
                store.push(account);
            }
            let mut off = Account::new("c", WEBDAV_TYPE);
            off.config = serde_json::json!({"server_url": "https://files.example/dav"});
            off.enabled = false;
            store.push(off);
            let loaded = from_accounts(&store, Some(home.path()));
            let a = loaded.iter().find(|x| x.id.as_str() == "a").unwrap();
            let b = loaded.iter().find(|x| x.id.as_str() == "b").unwrap();
            let c = loaded.iter().find(|x| x.id.as_str() == "c").unwrap();
            assert_eq!(a.mount, Some(home.path().join("Remote/Cloud")));
            assert_eq!(b.mount, Some(home.path().join("Remote/Cloud (2)")));
            assert_eq!(a.health, RemoteAccountHealth::NeverChecked);
            assert_eq!(c.health, RemoteAccountHealth::Disabled);
        }

        phase remote_account_projection_is_safe_and_identity_scoped;
        {
            {
                assert_eq!(
                    server_host("https://cloud.example/dav"),
                    Some("cloud.example".into())
                );
                assert_eq!(server_host("http://cloud.example"), None);
                assert_eq!(
                    server_host("http://127.0.0.1:8080/"),
                    Some("127.0.0.1:8080".to_owned()),
                    "plain HTTP to this machine, as the cloud service mounts it"
                );
                assert_eq!(server_host("https://user:pass@cloud.example"), None);
                assert_eq!(server_host("https://cloud.example/?token=secret"), None);
            }

            {
                assert_eq!(
                    resource_id(&AccountId::from("work/account")),
                    "files.remote-account:776f726b2f6163636f756e74"
                );
            }

            {
                let mut account = Account::new("cloud", NEXTCLOUD_TYPE);
                account.display_name = "Personal cloud".into();
                account.config = serde_json::json!({
                    "server_url": "https://cloud.example",
                    "username": "ada",
                    "password": "must-not-be-read",
                    "use_files": false,
                });
                assert!(from_account(&account).is_none());
            }

            {
                let mut account = Account::new("work", WEBDAV_TYPE);
                account.display_name = "Work files".into();
                account.config = serde_json::json!({
                    "server_url": "https://files.example/dav/ada",
                    "username": "ada",
                    "password": "must-not-be-read",
                });
                let projection = from_account(&account)
                    .expect("valid account projects");
                assert_eq!(projection.host, "files.example");
                assert_eq!(projection.label, "Work files");
                assert_eq!(projection.health, RemoteAccountHealth::NeverChecked);
            }
        }
    }
}
