//! Which provider serves an account.
//!
//! This is the one table that maps a Settings account to a
//! [`CloudSource`]. A new provider adds an arm to [`is_file_account`] and
//! [`spec_for`] (its account type and non-secret configuration) and to
//! [`open`]. Credentials never come from here: the service reads each
//! account's short-lived credential from the handoff store.
//!
//! Settings resolves an `OAuth` account's storage once at sign-in (the
//! `OneDrive` drive and root ids, a Dropbox team space root) and stores the
//! provider crate's own serde configuration in the account's non-secret
//! `config` under [`MOUNT_CONFIG_KEY`]. Without one, the provider's default
//! applies (and `OneDrive` resolves its drive when the mount starts).

use std::sync::Arc;

use accounts::cloud_types::{
    DROPBOX, GOOGLE_DRIVE, ICLOUD_DRIVE, MOUNT_CONFIG_KEY, NEXTCLOUD, ONEDRIVE, WEBDAV,
};
use cloud_source::{CloudSource, CredentialSource, Error, is_secure_endpoint};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Default resident bytes a mounted account may cache.
pub const DEFAULT_CACHE_BUDGET: u64 = 4 << 30;

/// Provider keys of a [`MountSpec`].
pub mod provider {
    /// Plain `WebDAV` and Nextcloud.
    pub const WEBDAV: &str = "webdav";
    pub const GOOGLE_DRIVE: &str = "google-drive";
    pub const ONEDRIVE: &str = "onedrive";
    pub const DROPBOX: &str = "dropbox";
    pub const ICLOUD_DRIVE: &str = "icloud-drive";
}

/// Everything a mount process needs besides its credential. Contains no
/// secret, so it may be logged and compared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountSpec {
    pub account_id: String,
    /// Provider key, one of [`provider`].
    pub provider: String,
    /// Provider-specific, non-secret configuration.
    pub config: serde_json::Value,
    pub cache_budget: u64,
    /// Start with background sync and uploads paused (the person paused
    /// the account); the service sets it when it starts the mount.
    #[serde(default)]
    pub paused: bool,
}

/// A `OneDrive` mount: resolved at sign-in, or still to be resolved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum OneDriveSpec {
    Resolved(cloud_source_onedrive::Mount),
    Unresolved(cloud_source_onedrive::OneDriveConfig),
}

/// Whether `account` is a file account, enabled or not. Folder names are
/// assigned over this set (Files names them the same way), so disabling an
/// account never renames another account's folder.
pub fn is_file_account(account: &accounts::Account) -> bool {
    let text = |key: &str| account.config.get(key).and_then(serde_json::Value::as_str);
    match account.type_id.as_str() {
        WEBDAV => text("server_url").is_some(),
        NEXTCLOUD => {
            text("server_url").is_some()
                && account
                    .config
                    .get("use_files")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true)
        }
        GOOGLE_DRIVE | ONEDRIVE | DROPBOX | ICLOUD_DRIVE => true,
        _ => false,
    }
}

/// The stored mount configuration of `account`, or `default` when Settings
/// stored none. `None` when the stored one is unusable.
fn stored<T: DeserializeOwned>(account: &accounts::Account, default: T) -> Option<T> {
    match account.config.get(MOUNT_CONFIG_KEY) {
        None | Some(serde_json::Value::Null) => Some(default),
        Some(value) => serde_json::from_value(value.clone()).ok(),
    }
}

/// Only endpoints credentials may be sent to.
fn secure<'a>(endpoints: impl IntoIterator<Item = &'a str>) -> Option<()> {
    endpoints.into_iter().all(is_secure_endpoint).then_some(())
}

fn json(config: &impl Serialize) -> Option<serde_json::Value> {
    serde_json::to_value(config).ok()
}

/// The provider key and configuration of a file account.
fn provider_config(account: &accounts::Account) -> Option<(&'static str, serde_json::Value)> {
    match account.type_id.as_str() {
        WEBDAV | NEXTCLOUD => {
            let (config, _) =
                cloud_source_dav::config_for_account(account.type_id.as_str(), &account.config)
                    .ok()?;
            Some((provider::WEBDAV, json(&config)?))
        }
        GOOGLE_DRIVE => {
            let config = stored(account, cloud_source_gdrive::DriveConfig::my_drive())?;
            secure([config.api.as_str()])?;
            Some((provider::GOOGLE_DRIVE, json(&config)?))
        }
        ONEDRIVE => {
            let config = stored(
                account,
                OneDriveSpec::Unresolved(cloud_source_onedrive::OneDriveConfig::me()),
            )?;
            let api = match &config {
                OneDriveSpec::Resolved(mount) => mount.api.as_str(),
                OneDriveSpec::Unresolved(config) => config.api.as_str(),
            };
            secure([api])?;
            Some((provider::ONEDRIVE, json(&config)?))
        }
        DROPBOX => {
            let config = stored(account, cloud_source_dropbox::DropboxConfig::new(""))?;
            let endpoints = &config.endpoints;
            secure([
                endpoints.api.as_str(),
                endpoints.content.as_str(),
                endpoints.notify.as_str(),
            ])?;
            Some((provider::DROPBOX, json(&config)?))
        }
        ICLOUD_DRIVE => {
            let config = stored(account, cloud_source_icloud::ICloudConfig::apple())?;
            secure([config.setup.as_str()])?;
            Some((provider::ICLOUD_DRIVE, json(&config)?))
        }
        _ => None,
    }
}

/// The mount specification of a Settings account, or `None` when it is
/// not a file account this build can mount (or is disabled).
pub fn spec_for(account: &accounts::Account) -> Option<MountSpec> {
    if !account.enabled {
        return None;
    }
    let (provider, config) = provider_config(account)?;
    Some(MountSpec {
        account_id: account.id.as_str().to_owned(),
        provider: provider.into(),
        config,
        cache_budget: DEFAULT_CACHE_BUDGET,
        paused: false,
    })
}

fn config<T: DeserializeOwned>(spec: &MountSpec) -> Result<T, Error> {
    serde_json::from_value(spec.config.clone())
        .map_err(|_| Error::InvalidResponse("mount configuration"))
}

/// Connect a provider for `spec`.
pub fn open(
    spec: &MountSpec,
    credentials: Arc<dyn CredentialSource>,
) -> Result<Arc<dyn CloudSource>, Error> {
    Ok(match spec.provider.as_str() {
        provider::WEBDAV => {
            let source = cloud_source_dav::DavSource::new(config(spec)?, credentials)?;
            // A server that offers sync-collection gets a change feed; if it
            // is unreachable now, polling works until the next start.
            if let Err(error) = source.probe() {
                tracing::info!(%error, "change-feed probe failed; polling");
            }
            Arc::new(source)
        }
        provider::GOOGLE_DRIVE => Arc::new(cloud_source_gdrive::GDrive::new(
            config(spec)?,
            credentials,
        )?),
        provider::ONEDRIVE => Arc::new(match config(spec)? {
            OneDriveSpec::Resolved(mount) => {
                cloud_source_onedrive::OneDriveSource::new(mount, credentials)?
            }
            OneDriveSpec::Unresolved(config) => {
                cloud_source_onedrive::OneDriveSource::connect(&config, credentials)?
            }
        }),
        provider::DROPBOX => Arc::new(cloud_source_dropbox::DropboxSource::new(
            config(spec)?,
            credentials,
        )?),
        provider::ICLOUD_DRIVE => Arc::new(cloud_source_icloud::ICloudDrive::new(
            config(spec)?,
            credentials,
        )?),
        _ => return Err(Error::Unsupported("provider")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(type_id: &str, config: serde_json::Value) -> accounts::Account {
        let mut account = accounts::Account::new("a", type_id);
        account.config = config;
        account
    }

    #[test]
    fn every_cloud_type_mounts_with_its_default_configuration() {
        for (type_id, key) in [
            (GOOGLE_DRIVE, provider::GOOGLE_DRIVE),
            (ONEDRIVE, provider::ONEDRIVE),
            (DROPBOX, provider::DROPBOX),
            (ICLOUD_DRIVE, provider::ICLOUD_DRIVE),
        ] {
            let spec = spec_for(&account(type_id, serde_json::json!({}))).expect(type_id);
            assert_eq!(spec.provider, key);
            assert!(is_file_account(&account(type_id, serde_json::json!({}))));
        }
    }

    #[test]
    fn a_resolved_configuration_is_used_as_stored() {
        let mount = cloud_source_onedrive::Mount {
            api: "https://graph.microsoft.com/v1.0/".into(),
            drive_id: "drive-1".into(),
            drive_type: cloud_source_onedrive::graph::DriveType::Personal,
            drive_root: "root-1".into(),
            root: "root-1".into(),
            upload_chunk: cloud_source_onedrive::DEFAULT_UPLOAD_CHUNK,
        };
        let spec = spec_for(&account(
            ONEDRIVE,
            serde_json::json!({ MOUNT_CONFIG_KEY: mount }),
        ))
        .unwrap();
        assert_eq!(
            serde_json::from_value::<cloud_source_onedrive::Mount>(spec.config).unwrap(),
            mount
        );
        let mut dropbox = cloud_source_dropbox::DropboxConfig::new("");
        dropbox.path_root = Some("1000".into());
        let spec = spec_for(&account(
            DROPBOX,
            serde_json::json!({ MOUNT_CONFIG_KEY: dropbox }),
        ))
        .unwrap();
        assert_eq!(
            serde_json::from_value::<cloud_source_dropbox::DropboxConfig>(spec.config)
                .unwrap()
                .path_root
                .as_deref(),
            Some("1000")
        );
    }

    #[test]
    fn credentials_are_never_sent_to_a_plain_http_remote_endpoint() {
        let drive = cloud_source_gdrive::DriveConfig::my_drive().with_api("http://drive.example/");
        assert!(
            spec_for(&account(
                GOOGLE_DRIVE,
                serde_json::json!({ MOUNT_CONFIG_KEY: drive })
            ))
            .is_none()
        );
        let local = cloud_source_gdrive::DriveConfig::my_drive().with_api("http://127.0.0.1:9/");
        assert!(
            spec_for(&account(
                GOOGLE_DRIVE,
                serde_json::json!({ MOUNT_CONFIG_KEY: local })
            ))
            .is_some()
        );
        assert!(
            spec_for(&account(
                DROPBOX,
                serde_json::json!({ MOUNT_CONFIG_KEY: "not a configuration" })
            ))
            .is_none()
        );
    }
}
