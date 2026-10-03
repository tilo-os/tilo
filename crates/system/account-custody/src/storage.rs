//! The non-secret mount configuration of a newly signed-in account.
//!
//! Right after an `OAuth` sign-in, Settings asks the provider once what the
//! account's storage is: the `OneDrive` drive and root folder ids, whether a
//! Dropbox member's calls are rooted in a team space. The answer is the
//! provider crate's own serde configuration, stored in the account's
//! non-secret `config` under [`accounts::cloud_types::MOUNT_CONFIG_KEY`], so
//! the storage daemon mounts the account without asking again.

use std::sync::Arc;

use cloud_source::{Credential, CredentialSource, StaticCredential};
use cloud_source_dropbox::{DropboxConfig, DropboxSource};
use cloud_source_gdrive::DriveConfig;
use cloud_source_onedrive::{OneDriveConfig, OneDriveSource};
use serde::Serialize;
use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

use crate::CustodyError;
use crate::providers::OAuthCloud;

fn base<T: DeserializeOwned>(
    base: Option<&serde_json::Value>,
    default: T,
) -> Result<T, CustodyError> {
    base.map_or(Ok(default), |value| {
        serde_json::from_value(value.clone())
            .map_err(|_| CustodyError::Account("invalid storage configuration".into()))
    })
}

fn value(config: &impl Serialize) -> Result<serde_json::Value, CustodyError> {
    serde_json::to_value(config)
        .map_err(|_| CustodyError::Account("invalid storage configuration".into()))
}

fn opened(cloud: OAuthCloud) -> impl Fn(cloud_source::Error) -> CustodyError {
    move |error| match error {
        cloud_source::Error::Offline => CustodyError::Offline,
        cloud_source::Error::Unauthorized => CustodyError::NeedsAuthentication,
        other => CustodyError::SignIn(format!("could not open your {}: {other}", cloud.label())),
    }
}

/// Resolve the mount configuration of `cloud` with a fresh access token.
/// `base` replaces the provider's default configuration (another API
/// origin, a chosen folder or shared drive).
///
/// # Errors
/// [`CustodyError::Offline`], [`CustodyError::NeedsAuthentication`] or
/// [`CustodyError::SignIn`] when the provider could not be asked.
pub fn resolve(
    cloud: OAuthCloud,
    base_config: Option<&serde_json::Value>,
    access_token: &str,
) -> Result<serde_json::Value, CustodyError> {
    let credentials: Arc<dyn CredentialSource> = Arc::new(StaticCredential(Credential::Bearer {
        token: Zeroizing::new(access_token.to_string()),
        expires: None,
    }));
    match cloud {
        // My Drive (or the configured shared drive or folder) is addressed
        // by aliases Drive resolves per request; nothing to ask.
        OAuthCloud::GoogleDrive => value(&base(base_config, DriveConfig::my_drive())?),
        OAuthCloud::OneDrive => {
            let config = base(base_config, OneDriveConfig::me())?;
            let source = OneDriveSource::connect(&config, credentials).map_err(opened(cloud))?;
            value(source.mount())
        }
        OAuthCloud::Dropbox => {
            let mut config = base(base_config, DropboxConfig::new(""))?;
            let source = DropboxSource::new(config.clone(), credentials).map_err(opened(cloud))?;
            config.path_root = source.team_root().map_err(opened(cloud))?;
            value(&config)
        }
    }
}
