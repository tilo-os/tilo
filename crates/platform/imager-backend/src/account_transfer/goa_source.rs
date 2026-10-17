//! Selected GOA refresh grants retain the original installed-app registration.
//! Source contracts:
//! https://github.com/GNOME/gnome-online-accounts/blob/master/src/goabackend/goautils.c
//! https://github.com/GNOME/gnome-online-accounts/blob/master/src/goabackend/goaoauth2provider.c
//! https://github.com/GNOME/gnome-online-accounts/blob/master/data/dbus-interfaces.xml
use super::{Candidate, ConnectionError, Connector, Origin, SourceRoots};
#[cfg(test)]
use account_custody::OAuthCloud;
use account_custody::{ImportedRegistration, Registration, TokenSet};
use accounts::{Account, AccountId, SecretVault};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};
use tilo_secrets::owner_migration::{GoaCredentials, GoaProvider, GoaRegistration};
use zeroize::{Zeroize, Zeroizing};

use crate::account_sources::{GoaDefinition as Definition, goa_definitions as definitions};

trait Keychain {
    fn read(
        &self,
        definition: &Definition,
        directory: &std::fs::File,
    ) -> Result<GoaCredentials, ConnectionError>;
}
struct NativeKeychain;
impl Keychain for NativeKeychain {
    fn read(
        &self,
        definition: &Definition,
        directory: &std::fs::File,
    ) -> Result<GoaCredentials, ConnectionError> {
        tilo_secrets::owner_migration::GoaCredential::new(
            directory,
            definition.provider,
            &definition.id,
            &definition.identity,
        )
        .and_then(|request| request.read_blocking())
        .map_err(|_| ConnectionError::CredentialUnavailable)
    }
}
struct PrivateTokens(TokenSet);
impl Drop for PrivateTokens {
    fn drop(&mut self) {
        self.0.access_token.zeroize();
        self.0.refresh_token.zeroize();
    }
}
fn original_registration(
    definition: &Definition,
    credentials: &GoaCredentials,
    tokens: &TokenSet,
) -> Result<ImportedRegistration, ConnectionError> {
    let explicit = definition
        .fields
        .get("OAuth2ClientId")
        .filter(|value| !value.is_empty());
    let fallback;
    let original = if let Some(runtime) = &credentials.registration {
        if explicit.is_some_and(|value| value != &runtime.client_id)
            || definition
                .fields
                .get("OAuth2ClientSecret")
                .is_some_and(|value| value != runtime.client_secret.as_str())
        {
            return Err(ConnectionError::AccountMissing);
        }
        runtime
    } else {
        fallback = GoaRegistration {
            client_id: explicit
                .cloned()
                .ok_or(ConnectionError::CredentialUnavailable)?,
            client_secret: Zeroizing::new(
                definition
                    .fields
                    .get("OAuth2ClientSecret")
                    .cloned()
                    .unwrap_or_default(),
            ),
        };
        &fallback
    };
    if definition.provider == GoaProvider::Google && original.client_secret.is_empty() {
        return Err(ConnectionError::CredentialUnavailable);
    }
    let mut client = Registration::public(original.client_id.clone());
    if !original.client_secret.is_empty() {
        client.client_secret = Some(original.client_secret.to_string());
    }
    let mut hash = Sha256::new();
    hash.update(definition.source_id().as_bytes());
    hash.update(tokens.refresh_token.as_bytes());
    ImportedRegistration::new(definition.cloud(), client, hex::encode(hash.finalize()))
        .map_err(super::connect::custody_error)
}
type SourceStamp = (u64, u64, u64, cap_std::time::SystemTime);
fn source_stamp(
    root: &crate::migration_directory::PinnedDirectory,
) -> Result<SourceStamp, ConnectionError> {
    use cap_primitives::fs::MetadataExt;
    root.validate()
        .map_err(|_| ConnectionError::AccountMissing)?;
    let facts = crate::nofollow::metadata_nofollow(&root.file, Path::new("goa-1.0/accounts.conf"))
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
    if !facts.is_file()
        || facts.uid() != rustix::process::geteuid().as_raw()
        || facts.mode() & 0o022 != 0
    {
        return Err(ConnectionError::CredentialUnavailable);
    }
    Ok((
        facts.dev(),
        facts.ino(),
        facts.len(),
        facts
            .modified()
            .map_err(|_| ConnectionError::AccountMissing)?,
    ))
}
fn validate_source(
    root: &crate::migration_directory::PinnedDirectory,
    stamp: &SourceStamp,
) -> Result<(), ConnectionError> {
    if &source_stamp(root)? != stamp {
        return Err(ConnectionError::AccountMissing);
    }
    Ok(())
}
impl Connector {
    /// Import one owner-consented GOA Google/OneDrive Files grant under its original client.
    /// # Errors
    /// Refuses changed source identity/registration, unsupported issuer overrides,
    /// unavailable original custody, mismatched remote identity or inaccessible files.
    pub fn import_goa(
        &self,
        roots: &SourceRoots,
        selected: &Candidate,
    ) -> Result<AccountId, ConnectionError> {
        self.import_goa_with(roots, selected, &NativeKeychain)
    }
    fn import_goa_with(
        &self,
        roots: &SourceRoots,
        selected: &Candidate,
        keychain: &impl Keychain,
    ) -> Result<AccountId, ConnectionError> {
        if selected.origin != Origin::Gnome
            || !selected.can_import_credentials
            || selected.profile != roots.config.join("goa-1.0/accounts.conf")
        {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let source = crate::migration_directory::PinnedDirectory::open_absolute(&roots.config)
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        use std::os::unix::fs::MetadataExt as _;
        let directory = source
            .file
            .metadata()
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        if directory.uid() != rustix::process::geteuid().as_raw() || directory.mode() & 0o022 != 0 {
            return Err(ConnectionError::CredentialUnavailable);
        }
        self.check_goa_destination(&source)?;
        let stamp = source_stamp(&source)?;
        let before = super::discovery::metadata(&roots.config, &selected.profile)
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .ok_or(ConnectionError::AccountMissing)?;
        let before = Zeroizing::new(before);
        let definition = definitions(&before, true)
            .into_iter()
            .find(|d| d.source_id() == selected.id)
            .ok_or(ConnectionError::AccountMissing)?;
        let current = definition.candidate(&selected.profile);
        if !definition.supported()
            || current.provider != selected.provider
            || current.identity != selected.identity
            || current.label != selected.label
        {
            return Err(ConnectionError::AccountMissing);
        }
        validate_source(&source, &stamp)?;
        let credentials = keychain.read(&definition, &source.file)?;
        self.recheck_goa(roots, selected, &before)?;
        validate_source(&source, &stamp)?;
        let tokens = PrivateTokens(
            super::goa_variant::parse(&Zeroizing::new(credentials.secret.expose().to_vec()))
                .map_err(|_| ConnectionError::CredentialUnavailable)?,
        );
        let registration = original_registration(&definition, &credentials, &tokens.0)?;
        let id = AccountId::from(format!("migrated-{}", definition.source_id()));
        if self.goa_already_imported(&id, &definition, &registration)? {
            return Ok(id);
        }
        let renewed = PrivateTokens(
            self.custodian
                .refresh_imported_cloud(definition.cloud(), &tokens.0, &registration)
                .map_err(super::connect::custody_error)?,
        );
        self.verify_goa_identity(&definition, &renewed.0)?;
        let mount = self
            .custodian
            .resolve_imported_storage(definition.cloud(), &renewed.0, None, None)
            .map_err(super::connect::custody_error)?;
        self.recheck_goa(roots, selected, &before)?;
        validate_source(&source, &stamp)?;
        let mut account = Account::new(id, definition.cloud().type_id());
        account.display_name.clone_from(&definition.label);
        account.config = serde_json::json!({"provider":definition.cloud().type_id(),"source_goa_identity":definition.identity.clone(),accounts::cloud_types::MOUNT_CONFIG_KEY:mount});
        self.persist_import(
            account,
            account_custody::imported_oauth_secret(&renewed.0, &registration),
        )
    }
    fn goa_already_imported(
        &self,
        id: &AccountId,
        definition: &Definition,
        registration: &ImportedRegistration,
    ) -> Result<bool, ConnectionError> {
        let store = super::connect::load_store(&self.root)?;
        if let Some(existing) = store.get(id) {
            if existing.type_id.as_str() != definition.cloud().type_id()
                || existing
                    .config
                    .get("source_goa_identity")
                    .and_then(serde_json::Value::as_str)
                    != Some(definition.identity.as_str())
            {
                return Err(ConnectionError::AlreadyPresent);
            }
            let secret = super::connect::PrivateSecret(
                SecretVault::open_read_only_in(&self.root)
                    .get(id)
                    .ok_or(ConnectionError::CredentialUnavailable)?,
            );
            if !account_custody::imported_oauth_matches(&secret.0, &registration) {
                return Err(ConnectionError::AlreadyPresent);
            }
            return Ok(true);
        }
        Ok(false)
    }
    fn check_goa_destination(
        &self,
        source: &crate::migration_directory::PinnedDirectory,
    ) -> Result<(), ConnectionError> {
        let mut existing = self.root.as_path();
        let target_exists = loop {
            match std::fs::symlink_metadata(existing) {
                Ok(_) => break existing == self.root,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    existing = existing
                        .parent()
                        .ok_or(ConnectionError::InvalidConfiguration)?;
                }
                Err(_) => return Err(ConnectionError::InvalidConfiguration),
            }
        };
        let target = crate::migration_directory::PinnedDirectory::open_absolute(existing)
            .map_err(|_| ConnectionError::InvalidConfiguration)?;
        if target.contains_identity(&source.identity)
            || (target_exists && source.contains_identity(&target.identity))
        {
            return Err(ConnectionError::InvalidConfiguration);
        }
        Ok(())
    }
    fn recheck_goa(
        &self,
        roots: &SourceRoots,
        selected: &Candidate,
        before: &str,
    ) -> Result<(), ConnectionError> {
        let after = Zeroizing::new(
            super::discovery::metadata(&roots.config, &selected.profile)
                .map_err(|_| ConnectionError::AccountMissing)?
                .ok_or(ConnectionError::AccountMissing)?,
        );
        if after.as_str() != before {
            return Err(ConnectionError::AccountMissing);
        }
        Ok(())
    }
    fn goa_json(&self, url: &str, token: &str) -> Result<serde_json::Value, ConnectionError> {
        #[cfg(test)]
        let endpoint = self.goa_api.as_deref().map(|base| {
            format!(
                "{base}{}",
                url::Url::parse(url)
                    .map(|url| url.path().to_owned())
                    .unwrap_or_default()
            )
        });
        #[cfg(test)]
        let url = endpoint.as_deref().unwrap_or(url);
        let response = tilo_net::http::blocking_client()
            .connect_timeout(std::time::Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ConnectionError::StorageUnavailable)?
            .get(url)
            .bearer_auth(token)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .map_err(|_| ConnectionError::SignInFailed)?;
        if !response.status().is_success() {
            return Err(ConnectionError::CredentialUnavailable);
        }
        let mut bytes = Vec::new();
        response
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ConnectionError::SignInFailed)?;
        if bytes.len() > 64 * 1024 {
            return Err(ConnectionError::SignInFailed);
        }
        serde_json::from_slice(&bytes).map_err(|_| ConnectionError::SignInFailed)
    }
    fn verify_goa_identity(
        &self,
        definition: &Definition,
        tokens: &TokenSet,
    ) -> Result<(), ConnectionError> {
        let (endpoint, key) = match definition.provider {
            GoaProvider::Google => ("https://www.googleapis.com/oauth2/v2/userinfo", "email"),
            GoaProvider::Microsoft => ("https://graph.microsoft.com/v1.0/me", "mail"),
        };
        let profile = self.goa_json(endpoint, &tokens.access_token)?;
        if profile.get(key).and_then(serde_json::Value::as_str)
            != Some(definition.identity.as_str())
        {
            return Err(ConnectionError::AccountMissing);
        }
        if definition.provider == GoaProvider::Google {
            // DriveConfig aliases alone perform no remote permission check.
            let root = self.goa_json(
                "https://www.googleapis.com/drive/v3/files/root?fields=id",
                &tokens.access_token,
            )?;
            if root
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err(ConnectionError::CredentialUnavailable);
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
