//! Selected rclone user refresh grants retain their original client registration.
//! Provenance: https://github.com/rclone/rclone/blob/master/lib/oauthutil/oauthutil.go
//! Provider defaults: https://github.com/rclone/rclone/tree/master/backend
//! No arbitrary token endpoints, service accounts, sovereign clouds or app-only grants.
use std::collections::BTreeMap;

use account_custody::{ImportedRegistration, OAuthCloud, Registration, TokenSet};
use accounts::{Account, SecretVault};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use super::{Candidate, ConnectionError, Connector, Inventory, Origin, Provider, SourceRoots};

fn cloud(provider: Provider) -> Option<OAuthCloud> {
    match provider {
        Provider::Google => Some(OAuthCloud::GoogleDrive),
        Provider::Microsoft => Some(OAuthCloud::OneDrive),
        Provider::Dropbox => Some(OAuthCloud::Dropbox),
        _ => None,
    }
}

// These are published rclone installed-app defaults, not a Tilo registration or
// a user's secret. Explicit client_id clears the default secret upstream.
fn defaults(cloud: OAuthCloud) -> (&'static str, &'static str) {
    match cloud {
        OAuthCloud::GoogleDrive => (
            "202264815644.apps.googleusercontent.com",
            "X4Z3ca8xfWDb1Voo-F9a7ZxJ",
        ),
        OAuthCloud::OneDrive => (
            "b15665d9-eda6-4092-8539-0eec376afd59",
            "qtyfaBBYA403=unZUP40~_#",
        ),
        OAuthCloud::Dropbox => ("5jcck7diasz0rqy", "1n9m04y2zx7bf26"),
    }
}

fn section<'a>(text: &'a str, label: &str) -> Result<BTreeMap<&'a str, &'a str>, ConnectionError> {
    let mut active = false;
    let mut found = false;
    let mut values = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            active = name == label;
            if active && found {
                return Err(ConnectionError::InvalidConfiguration);
            }
            found |= active;
        } else if active && let Some((key, value)) = line.split_once('=') {
            if values.insert(key.trim(), value.trim()).is_some() {
                return Err(ConnectionError::InvalidConfiguration);
            }
        }
    }
    if !found {
        return Err(ConnectionError::AccountMissing);
    }
    Ok(values)
}

use crate::account_sources::rclone_supported as supported;

fn registration(
    cloud: OAuthCloud,
    values: &BTreeMap<&str, &str>,
    binding: String,
) -> Result<ImportedRegistration, ConnectionError> {
    let public: BTreeMap<String, String> = values
        .iter()
        .filter(|(key, _)| {
            matches!(
                **key,
                "auth_url"
                    | "token_url"
                    | "client_credentials"
                    | "service_account_file"
                    | "service_account_credentials"
                    | "impersonate"
                    | "tenant_url"
                    | "team_member"
                    | "resource_key"
                    | "region"
                    | "tenant"
                    | "scope"
            )
        })
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect();
    if !supported(&public) {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let (default_id, default_secret) = defaults(cloud);
    let explicit_id = values.get("client_id").filter(|id| !id.is_empty());
    let mut client = Registration::public(explicit_id.copied().unwrap_or(default_id));
    client.client_secret = if explicit_id.is_none() {
        Some(default_secret.into())
    } else {
        None
    };
    if let Some(secret) = values
        .get("client_secret")
        .filter(|secret| !secret.is_empty())
    {
        // rclone's client_secret config is verbatim, NOT its obscure password format.
        client.client_secret = Some((*secret).into());
    }
    ImportedRegistration::new(cloud, client, binding)
        .map_err(|_| ConnectionError::InvalidConfiguration)
}

struct PrivateTokens(TokenSet);
impl Drop for PrivateTokens {
    fn drop(&mut self) {
        self.0.access_token.zeroize();
        self.0.refresh_token.zeroize();
    }
}

#[derive(serde::Deserialize)]
struct SourceToken {
    #[serde(default, alias = "RefreshToken")]
    refresh_token: String,
    #[serde(default)]
    token_type: Option<String>,
}
impl Drop for SourceToken {
    fn drop(&mut self) {
        self.refresh_token.zeroize();
    }
}

fn tokens(values: &BTreeMap<&str, &str>) -> Result<PrivateTokens, ConnectionError> {
    let text = values
        .get("token")
        .ok_or(ConnectionError::CredentialUnavailable)?;
    if text.len() > 32 * 1024 {
        return Err(ConnectionError::CredentialUnavailable);
    }
    let mut value: SourceToken =
        serde_json::from_str(text).map_err(|_| ConnectionError::CredentialUnavailable)?;
    if value.refresh_token.is_empty()
        || value.refresh_token.len() > 16 * 1024
        || value.refresh_token.chars().any(char::is_control)
        || value
            .token_type
            .as_deref()
            .is_some_and(|s| !s.eq_ignore_ascii_case("bearer"))
    {
        return Err(ConnectionError::CredentialUnavailable);
    }
    // Never restart a source lifetime or claim configured scopes were granted.
    Ok(PrivateTokens(TokenSet {
        refresh_token: std::mem::take(&mut value.refresh_token),
        ..TokenSet::default()
    }))
}

#[cfg(unix)]
struct SourceWitness {
    root: std::sync::Arc<crate::migration_directory::PinnedDirectory>,
    stamp: (u64, u64, u64, cap_std::time::SystemTime),
}
#[cfg(unix)]
impl SourceWitness {
    fn open(roots: &SourceRoots) -> Result<Self, ConnectionError> {
        let root = crate::migration_directory::PinnedDirectory::open_absolute(&roots.config)
            .map_err(|_| ConnectionError::AccountMissing)?;
        let stamp = Self::stamp(&root)?;
        Ok(Self { root, stamp })
    }
    fn stamp(
        root: &crate::migration_directory::PinnedDirectory,
    ) -> Result<(u64, u64, u64, cap_std::time::SystemTime), ConnectionError> {
        use cap_primitives::fs::MetadataExt;
        root.validate()
            .map_err(|_| ConnectionError::AccountMissing)?;
        let m = crate::nofollow::metadata_nofollow(
            &root.file,
            std::path::Path::new("rclone/rclone.conf"),
        )
        .map_err(|_| ConnectionError::AccountMissing)?;
        if m.uid() != rustix::process::geteuid().as_raw() || !m.is_file() || m.mode() & 0o022 != 0 {
            return Err(ConnectionError::CredentialUnavailable);
        }
        Ok((
            m.dev(),
            m.ino(),
            m.len(),
            m.modified().map_err(|_| ConnectionError::AccountMissing)?,
        ))
    }
    fn validate(&self) -> Result<(), ConnectionError> {
        if Self::stamp(&self.root)? != self.stamp {
            return Err(ConnectionError::AccountMissing);
        }
        Ok(())
    }
}

impl Connector {
    pub(super) fn import_rclone_oauth(
        &self,
        roots: &SourceRoots,
        selected: &Candidate,
    ) -> Result<accounts::AccountId, ConnectionError> {
        let cloud = cloud(selected.provider).ok_or(ConnectionError::InvalidConfiguration)?;
        if selected.origin != Origin::Rclone
            || selected.profile != roots.config.join("rclone/rclone.conf")
        {
            return Err(ConnectionError::InvalidConfiguration);
        }
        #[cfg(unix)]
        let witness = SourceWitness::open(roots)?;
        let text = super::discovery::metadata(&roots.config, &selected.profile)
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .ok_or(ConnectionError::AccountMissing)?;
        let mut inventory = Inventory::default();
        super::discovery::rclone(&text, &selected.profile, &mut inventory);
        let current = inventory
            .candidates
            .into_iter()
            .find(|item| {
                item.id == selected.id
                    && item.provider == selected.provider
                    && item.label == selected.label
                    && item.server == selected.server
                    && item.identity == selected.identity
                    && item.can_import_credentials
            })
            .ok_or(ConnectionError::AccountMissing)?;
        let values = section(&text, &selected.label)?;
        let source_tokens = tokens(&values)?;
        let mut digest = Sha256::new();
        digest.update(source_tokens.0.refresh_token.as_bytes());
        let registration = registration(cloud, &values, hex::encode(digest.finalize()))?;
        let mut identity = Sha256::new();
        identity.update(current.profile.as_os_str().as_encoded_bytes());
        identity.update(current.id.as_bytes());
        let id = accounts::AccountId::new(format!(
            "migrated-rclone-oauth-{}",
            hex::encode(identity.finalize())
        ));
        let store = super::connect::load_store(&self.root)?;
        if let Some(existing) = store.get(&id) {
            if existing.type_id.as_str() != cloud.type_id()
                || existing.display_name != current.label
            {
                return Err(ConnectionError::AlreadyPresent);
            }
            let vault = SecretVault::open_in(&self.root);
            let secret = vault
                .get(&id)
                .ok_or(ConnectionError::CredentialUnavailable)?;
            if !account_custody::imported_oauth_matches(&secret, &registration) {
                return Err(ConnectionError::AlreadyPresent);
            }
            self.recheck_rclone(roots, selected, &text)?;
            #[cfg(unix)]
            witness.validate()?;
            return Ok(id);
        }
        let renewed = PrivateTokens(
            self.custodian
                .refresh_imported_cloud(cloud, &source_tokens.0, &registration)
                .map_err(super::connect::custody_error)?,
        );
        let mount = self
            .custodian
            .resolve_imported_storage(
                cloud,
                &renewed.0,
                values
                    .get("root_folder_id")
                    .copied()
                    .filter(|s| !s.is_empty()),
                values
                    .get(if cloud == OAuthCloud::GoogleDrive {
                        "team_drive"
                    } else {
                        "drive_id"
                    })
                    .copied()
                    .filter(|s| !s.is_empty()),
            )
            .map_err(super::connect::custody_error)?;
        self.recheck_rclone(roots, selected, &text)?;
        #[cfg(unix)]
        witness.validate()?;
        let mut account = Account::new(id, cloud.type_id());
        account.display_name = current.label;
        account.config = serde_json::json!({"provider":cloud.type_id(), accounts::cloud_types::MOUNT_CONFIG_KEY:mount});
        self.persist_import(
            account,
            account_custody::imported_oauth_secret(&renewed.0, &registration),
        )
    }

    fn recheck_rclone(
        &self,
        roots: &SourceRoots,
        selected: &Candidate,
        text: &str,
    ) -> Result<(), ConnectionError> {
        let current = super::discovery::metadata(&roots.config, &selected.profile)
            .map_err(|_| ConnectionError::AccountMissing)?
            .ok_or(ConnectionError::AccountMissing)?;
        if current.as_str() != text {
            return Err(ConnectionError::AccountMissing);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
