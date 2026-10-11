//! Read-only projection of the canonical external accounts that can provide
//! files.  Settings owns setup and credential custody; the storage daemon's
//! cloud service mounts each enabled account at `~/Remote/<label>` and
//! reports its health. Files owns neither: it shows each account as the
//! folder the service mounts, with the reported health as a badge.
//!
//! This projection never opens [`accounts::SecretVault`] and never talks to
//! the server, so it is safe to expose to people, search, and agents.

use std::path::{Path, PathBuf};

use accounts::{Account, AccountId, AccountSyncHealth, AccountSyncStatusStore, AccountTypeId};
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

/// A non-secret, bounded Files-facing account descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAccount {
    pub id: AccountId,
    pub kind: RemoteAccountKind,
    pub label: String,
    pub host: String,
    pub health: RemoteAccountHealth,
    /// Where the cloud service mounts the account (`~/Remote/<label>`).
    /// The folder exists only while the account is mounted.
    pub mount: Option<PathBuf>,
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
            Self::Syncing => "Syncing",
            Self::Healthy => "Up to date",
            Self::Offline => "Offline",
            Self::NeedsAuthentication => "Sign-in required",
            Self::Error => "Needs attention",
        }
    }

    /// Human-safe guidance about an account's folder. It deliberately says
    /// nothing about credentials or server responses.
    pub fn unavailable_reason(self) -> &'static str {
        match self {
            Self::Disabled => "This account is disabled in Settings",
            Self::Offline => {
                "Offline: files read before stay available and changes upload when the connection returns"
            }
            Self::NeedsAuthentication => "Sign in again in Settings to keep this folder in sync",
            Self::Error => "This account needs attention in Settings",
            Self::NeverChecked => "The account has not been checked yet",
            Self::Syncing => "This folder is syncing",
            Self::Healthy => "This folder is up to date",
        }
    }

    /// A short badge for the account's folder; `None` when all is well.
    pub fn badge(self) -> Option<&'static str> {
        match self {
            Self::Healthy => None,
            Self::Disabled => Some("Disabled"),
            Self::Offline => Some("Offline"),
            Self::NeedsAuthentication => Some("Sign in"),
            Self::Error => Some("Attention"),
            Self::NeverChecked | Self::Syncing => Some("Syncing"),
        }
    }
}

/// Load the externally configured file providers from the user's account
/// store, mounted under the user's home.
pub fn load() -> Vec<RemoteAccount> {
    match accounts::config_root() {
        Some(root) => load_in(&root, crate::model::user_home().as_deref()),
        None => Vec::new(),
    }
}

/// Load from an explicit Settings config root, with mounts under
/// `home/Remote`. DAV URLs with userinfo/query or a non-HTTPS scheme are
/// withheld. OAuth providers use fixed public service labels rather than
/// copying configured authorization endpoints or credentials into the view.
pub fn load_in(config_root: &Path, home: Option<&Path>) -> Vec<RemoteAccount> {
    let store = accounts::AccountStore::load_in(config_root);
    let health = AccountSyncStatusStore::load_in(config_root);
    let mut projected: Vec<RemoteAccount> = store
        .all()
        .iter()
        .filter_map(|account| from_account(account, &health))
        .take(MAX_REMOTE_ACCOUNTS)
        .collect();
    if let Some(home) = home {
        assign_mounts(&mut projected, &store, &home.join(MOUNTS_DIR));
    }
    projected
}

/// The folder under the home directory the cloud service mounts into.
pub const MOUNTS_DIR: &str = "Remote";

/// Name each account's folder exactly as the cloud service does: every
/// file account's label (enabled or not), made unique in account-id order.
fn assign_mounts(projected: &mut [RemoteAccount], store: &accounts::AccountStore, root: &Path) {
    let labels: Vec<(&str, &str)> = store
        .all()
        .iter()
        .filter(|account| is_file_account(account))
        .map(|account| (account.id.as_str(), account.display_name.as_str()))
        .collect();
    let names = cloud_source::tree::mount_names(&labels);
    for account in projected.iter_mut() {
        account.mount = names
            .iter()
            .find(|(id, _)| id == account.id.as_str())
            .map(|(_, name)| root.join(name));
    }
}

/// Whether the cloud service considers `account` a mountable file account.
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

fn from_account(account: &Account, statuses: &AccountSyncStatusStore) -> Option<RemoteAccount> {
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
            .and_then(safe_https_host)?,
    };
    let label = bounded_label(if account.display_name.trim().is_empty() {
        kind.label()
    } else {
        account.display_name.as_str()
    });
    let health = if account.enabled {
        match statuses.get(&account.id).map(|status| status.health) {
            Some(AccountSyncHealth::Syncing) => RemoteAccountHealth::Syncing,
            Some(AccountSyncHealth::Healthy) => RemoteAccountHealth::Healthy,
            Some(AccountSyncHealth::Offline) => RemoteAccountHealth::Offline,
            Some(AccountSyncHealth::NeedsAuthentication) => {
                RemoteAccountHealth::NeedsAuthentication
            }
            Some(AccountSyncHealth::Error) => RemoteAccountHealth::Error,
            Some(AccountSyncHealth::Never) | None => RemoteAccountHealth::NeverChecked,
        }
    } else {
        RemoteAccountHealth::Disabled
    };
    Some(RemoteAccount {
        id: account.id.clone(),
        kind,
        label,
        host,
        health,
        mount: None,
    })
}

/// Return a display host only for an origin that is safe to present as a
/// remote endpoint. This is intentionally small rather than accepting every
/// URL form: account setup owns discovery and redirect policy.
fn safe_https_host(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix("https://")?;
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

/// Stable, non-path identity used by the resource tree and search. Account
/// ids can contain punctuation, so hex keeps action-address parsing simple.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_links_use_cloud_daemon_names_when_oauth_provider_labels_collide() {
        for provider in ["google-drive", "onedrive", "dropbox", "icloud-drive"] {
            let root = tempfile::tempdir().unwrap();
            let home = tempfile::tempdir().unwrap();
            let mut store = accounts::AccountStore::new();
            let mut oauth = Account::new("a-cloud", provider);
            oauth.display_name = "Cloud".into();
            store.upsert(oauth);
            let mut dav = Account::new("z-dav", WEBDAV_TYPE);
            dav.display_name = "Cloud".into();
            dav.config = serde_json::json!({"server_url":"https://files.example/dav"});
            store.upsert(dav);
            store.save_in(root.path()).unwrap();
            // The storage daemon reserves mount names for every file provider,
            // including disabled accounts. The Files resource must open DAV,
            // rather than the earlier provider's identically named folder.
            let loaded = load_in(root.path(), Some(home.path()));
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

    #[test]
    fn accounts_load_from_an_explicit_root_with_their_mount_folders() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut store = accounts::AccountStore::new();
        for (id, label) in [("b", "Cloud"), ("a", "Cloud")] {
            let mut account = Account::new(id, WEBDAV_TYPE);
            account.display_name = label.into();
            account.config = serde_json::json!({"server_url": "https://files.example/dav"});
            store.upsert(account);
        }
        store.save_in(root.path()).unwrap();
        accounts::AccountSyncReporter::in_root(AccountId::from("a"), root.path())
            .offline()
            .unwrap();
        let loaded = load_in(root.path(), Some(home.path()));
        let a = loaded.iter().find(|x| x.id.as_str() == "a").unwrap();
        let b = loaded.iter().find(|x| x.id.as_str() == "b").unwrap();
        assert_eq!(a.mount, Some(home.path().join("Remote/Cloud")));
        assert_eq!(b.mount, Some(home.path().join("Remote/Cloud (2)")));
        assert_eq!(a.health, RemoteAccountHealth::Offline);
        assert_eq!(a.health.badge(), Some("Offline"));
        assert_eq!(b.health, RemoteAccountHealth::NeverChecked);
    }

    /// Only safe HTTPS origins are presented, identities never use raw ids, Nextcloud without files is no resource and no credentials are projected.
    #[test]
    fn remote_account_projection_is_safe_and_identity_scoped() {
        // --- phase: only_safe_https_origins_are_presented ---
        {
            assert_eq!(
                safe_https_host("https://cloud.example/dav"),
                Some("cloud.example".into())
            );
            assert_eq!(safe_https_host("http://cloud.example"), None);
            assert_eq!(safe_https_host("https://user:pass@cloud.example"), None);
            assert_eq!(safe_https_host("https://cloud.example/?token=secret"), None);
        }

        // --- phase: resource_identity_never_uses_the_raw_account_id ---
        {
            assert_eq!(
                resource_id(&AccountId::from("work/account")),
                "files.remote-account:776f726b2f6163636f756e74"
            );
        }

        // --- phase: nextcloud_without_files_is_not_a_files_resource ---
        {
            let mut account = Account::new("cloud", NEXTCLOUD_TYPE);
            account.display_name = "Personal cloud".into();
            account.config = serde_json::json!({
                "server_url": "https://cloud.example",
                "username": "ada",
                "password": "must-not-be-read",
                "use_files": false,
            });
            assert!(from_account(&account, &AccountSyncStatusStore::default()).is_none());
        }

        // --- phase: projection_uses_no_username_or_password ---
        {
            let mut account = Account::new("work", WEBDAV_TYPE);
            account.display_name = "Work files".into();
            account.config = serde_json::json!({
                "server_url": "https://files.example/dav/ada",
                "username": "ada",
                "password": "must-not-be-read",
            });
            let projection = from_account(&account, &AccountSyncStatusStore::default())
                .expect("valid account projects");
            assert_eq!(projection.host, "files.example");
            assert_eq!(projection.label, "Work files");
            assert_eq!(projection.health, RemoteAccountHealth::NeverChecked);
        }
    }
}
