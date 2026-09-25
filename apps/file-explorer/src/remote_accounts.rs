//! Read-only projection of the canonical external accounts that can provide
//! files.  Settings owns setup and credential custody; Files owns neither.
//!
//! In particular this *projection* never opens [`accounts::SecretVault`].
//! The explicit `WebDAV` transport owns the narrowly-scoped, on-demand vault
//! handoff; this model remains safe to expose to people, search, and agents
//! without inventing a second account schema or replaying a credential-bearing
//! URL.

use accounts::{Account, AccountId, AccountSyncHealth, AccountSyncStatusStore, AccountTypeId};
use serde_json::Value;

pub const WEBDAV_TYPE: &str = "webdav";
pub const NEXTCLOUD_TYPE: &str = "nextcloud";
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAccountKind {
    WebDav,
    Nextcloud,
}

impl RemoteAccountKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::WebDav => "WebDAV",
            Self::Nextcloud => "Nextcloud",
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
            Self::Syncing => "Connecting",
            Self::Healthy => "Ready to mount",
            Self::Offline => "Offline",
            Self::NeedsAuthentication => "Sign-in required",
            Self::Error => "Needs attention",
        }
    }

    /// Human-safe guidance for a remote account that cannot presently be
    /// browsed. A real `WebDAV` browse is available when the account is ready;
    /// this deliberately says nothing about credentials or server responses.
    pub fn unavailable_reason(self) -> &'static str {
        match self {
            Self::Disabled => "This account is disabled in Settings",
            Self::Offline => "This account is waiting for a network connection",
            Self::NeedsAuthentication => "Sign in again in Settings before opening files",
            Self::Error => "This account needs attention in Settings",
            Self::NeverChecked => "The account has not been checked yet",
            Self::Syncing => "This account is connecting",
            Self::Healthy => "The remote file account is ready",
        }
    }
}

/// Load the externally configured file providers from the one shared store.
/// Invalid or unsuitable records are withheld: URLs with userinfo/query or a
/// non-HTTPS scheme are never presented as mount destinations.
pub fn load() -> Vec<RemoteAccount> {
    let store = accounts::AccountStore::load();
    let health = AccountSyncStatusStore::load();
    store
        .all()
        .iter()
        .filter_map(|account| from_account(account, &health))
        .take(MAX_REMOTE_ACCOUNTS)
        .collect()
}

pub fn find(id: &str) -> Option<RemoteAccount> {
    load().into_iter().find(|account| account.id.as_str() == id)
}

fn from_account(account: &Account, statuses: &AccountSyncStatusStore) -> Option<RemoteAccount> {
    let kind = match account.type_id.as_str() {
        WEBDAV_TYPE => RemoteAccountKind::WebDav,
        NEXTCLOUD_TYPE => RemoteAccountKind::Nextcloud,
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
    let host = account
        .config
        .get("server_url")
        .and_then(Value::as_str)
        .and_then(safe_https_host)?;
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_safe_https_origins_are_presented() {
        assert_eq!(
            safe_https_host("https://cloud.example/dav"),
            Some("cloud.example".into())
        );
        assert_eq!(safe_https_host("http://cloud.example"), None);
        assert_eq!(safe_https_host("https://user:pass@cloud.example"), None);
        assert_eq!(safe_https_host("https://cloud.example/?token=secret"), None);
    }

    #[test]
    fn resource_identity_never_uses_the_raw_account_id() {
        assert_eq!(
            resource_id(&AccountId::from("work/account")),
            "files.remote-account:776f726b2f6163636f756e74"
        );
    }

    #[test]
    fn nextcloud_without_files_is_not_a_files_resource() {
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

    #[test]
    fn projection_uses_no_username_or_password() {
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
