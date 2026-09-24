//! Which provider serves an account, and with what credential.
//!
//! This is the one table that maps a Settings account to a
//! [`CloudSource`]. A new provider adds an arm to [`spec_for`] (its account
//! types and non-secret configuration), to [`credential_for`] (which part
//! of the vault secret it uses) and to [`open`].

use std::sync::Arc;

use cloud_source::{CloudSource, Credential, CredentialSource, Error};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Default resident bytes a mounted account may cache.
pub const DEFAULT_CACHE_BUDGET: u64 = 4 << 30;

/// Everything a mount process needs besides its credential. Contains no
/// secret, so it may be logged and compared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountSpec {
    pub account_id: String,
    /// Provider key: `"webdav"` (plain `WebDAV` and Nextcloud).
    pub provider: String,
    /// Provider-specific, non-secret configuration.
    pub config: serde_json::Value,
    pub cache_budget: u64,
}

/// Whether `account` is a file account, enabled or not. Folder names are
/// assigned over this set (Files names them the same way), so disabling an
/// account never renames another account's folder.
pub fn is_file_account(account: &accounts::Account) -> bool {
    let text = |key: &str| account.config.get(key).and_then(serde_json::Value::as_str);
    match account.type_id.as_str() {
        cloud_source_dav::account::WEBDAV_TYPE => text("server_url").is_some(),
        cloud_source_dav::account::NEXTCLOUD_TYPE => {
            text("server_url").is_some()
                && account
                    .config
                    .get("use_files")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true)
        }
        _ => false,
    }
}

/// The mount specification of a Settings account, or `None` when it is
/// not a file account this build can mount (or is disabled).
pub fn spec_for(account: &accounts::Account) -> Option<MountSpec> {
    if !account.enabled {
        return None;
    }
    match account.type_id.as_str() {
        cloud_source_dav::account::WEBDAV_TYPE | cloud_source_dav::account::NEXTCLOUD_TYPE => {
            let (config, _) =
                cloud_source_dav::config_for_account(account.type_id.as_str(), &account.config)
                    .ok()?;
            Some(MountSpec {
                account_id: account.id.as_str().to_owned(),
                provider: "webdav".into(),
                config: serde_json::to_value(config).ok()?,
                cache_budget: DEFAULT_CACHE_BUDGET,
            })
        }
        _ => None,
    }
}

/// The credential a provider needs from an account's vault secret. Only
/// this, never the vault, reaches the mount process.
pub fn credential_for(
    account: &accounts::Account,
    secret: &accounts::Secret,
) -> Option<Credential> {
    match account.type_id.as_str() {
        cloud_source_dav::account::WEBDAV_TYPE | cloud_source_dav::account::NEXTCLOUD_TYPE => {
            if let Some(token) = secret.token.as_ref().filter(|t| !t.is_empty()) {
                return Some(Credential::Bearer {
                    token: Zeroizing::new(token.clone()),
                    expires: None,
                });
            }
            if secret.password.is_empty() {
                return None;
            }
            let username = account
                .config
                .get("username")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Some(Credential::Basic {
                username,
                password: Zeroizing::new(secret.password.clone()),
            })
        }
        _ => None,
    }
}

/// Connect a provider for `spec`.
pub fn open(
    spec: &MountSpec,
    credentials: Arc<dyn CredentialSource>,
) -> Result<Arc<dyn CloudSource>, Error> {
    match spec.provider.as_str() {
        "webdav" => {
            let config: cloud_source_dav::DavConfig =
                serde_json::from_value(spec.config.clone())
                    .map_err(|_| Error::InvalidResponse("mount configuration"))?;
            let source = cloud_source_dav::DavSource::new(config, credentials)?;
            // A server that offers sync-collection gets a change feed; if it
            // is unreachable now, polling works until the next start.
            if let Err(error) = source.probe() {
                tracing::info!(%error, "change-feed probe failed; polling");
            }
            Ok(Arc::new(source))
        }
        _ => Err(Error::Unsupported("provider")),
    }
}
