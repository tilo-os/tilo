use std::path::{Path, PathBuf};

use account_credentials::{Authorization, CredentialError, CredentialStore};
use accounts::{Account, AccountId, Secret, SecretVault};
use zeroize::Zeroizing;

use crate::account_store::Accounts;
pub use crate::account_store::ConnectionError;
pub(super) use crate::account_store::PrivateSecret;

#[derive(Clone, Copy)]
enum ImportPurpose {
    CopySource,
    PreparedHandoff,
}

/// A private destination configuration root: a staged migration copy or the target user's
/// installed home.
#[derive(Clone)]
pub struct Connector {
    pub(super) root: PathBuf,
    pub(super) credential_store: CredentialStore,
    /// The destination's accounts record as it stood when this connector was made, or why it did
    /// not read. Each edit this connector lands goes into it.
    pub(super) accounts: Result<Accounts, ConnectionError>,
    #[cfg(any(test, feature = "test-support"))]
    pub(super) spotify_api: Option<String>,
}

impl Connector {
    pub fn new(root: PathBuf) -> Self {
        let credential_store = CredentialStore::for_root(Some(root.clone()));
        let accounts = Accounts::read(&root);
        Self {
            root,
            credential_store,
            accounts,
            #[cfg(any(test, feature = "test-support"))]
            spotify_api: None,
        }
    }

    pub fn with_credential_store(root: PathBuf, credential_store: CredentialStore) -> Self {
        let accounts = Accounts::read(&root);
        Self {
            root,
            credential_store,
            accounts,
            #[cfg(any(test, feature = "test-support"))]
            spotify_api: None,
        }
    }

    /// The destination's accounts record, or why it did not read.
    ///
    /// # Errors
    /// The projection it was made from did not read.
    pub fn accounts(&self) -> Result<&Accounts, ConnectionError> {
        self.accounts
            .as_ref()
            .map_err(|_| ConnectionError::StorageUnavailable)
    }

    /// Uses the exact grant slot Tilo Music consumes; streaming stays a separate consent.
    pub fn begin_spotify(&self) -> Result<Authorization, ConnectionError> {
        use account_credentials::{Capability, OAuthIssuer, OAuthUse};
        self.credential_store
            .begin_sign_in_for_scopes(
                OAuthUse::new(OAuthIssuer::Spotify, Capability::Music),
                &["user-read-private", "user-library-read"],
                None,
            )
            .map_err(credential_error)
    }

    pub fn finish_spotify(
        &self,
        authorization: Authorization,
    ) -> Result<AccountId, ConnectionError> {
        let id = AccountId::from("music.spotify");
        if self.accounts()?.get(id.as_str())?.is_some() {
            return Err(ConnectionError::AlreadyPresent);
        }
        let mut tokens =
            CredentialStore::finish_sign_in(authorization).map_err(credential_error)?;
        if tokens.scope.is_none() {
            tokens.scope = Some("user-read-private user-library-read".into());
        }
        self.persist_verified_spotify(id, tokens, false)
    }

    /// Separate playback consent for the same slot.
    pub fn begin_spotify_streaming(&self) -> Result<Authorization, ConnectionError> {
        use account_credentials::{Capability, OAuthIssuer, OAuthUse};
        self.credential_store
            .begin_sign_in_for_scopes(
                OAuthUse::new(OAuthIssuer::Spotify, Capability::MusicStreaming),
                &["streaming", "user-read-private"],
                None,
            )
            .map_err(credential_error)
    }

    pub fn finish_spotify_streaming(
        &self,
        authorization: Authorization,
    ) -> Result<AccountId, ConnectionError> {
        let id = AccountId::from("music.spotify.streaming");
        if self.accounts()?.get(id.as_str())?.is_some() {
            return Err(ConnectionError::AlreadyPresent);
        }
        let mut tokens =
            CredentialStore::finish_sign_in(authorization).map_err(credential_error)?;
        if tokens.scope.is_none() {
            tokens.scope = Some("streaming user-read-private".into());
        }
        if !tokens.scope.as_deref().is_some_and(|scope| {
            let mut granted = scope.split_ascii_whitespace();
            granted.any(|value| value == "streaming")
                && scope
                    .split_ascii_whitespace()
                    .any(|value| value == "user-read-private")
        }) {
            return Err(ConnectionError::SignInFailed);
        }
        self.persist_verified_spotify(id, tokens, true)
    }

    /// The person's ordinary password never enters the installer; this is the device's revocable app
    /// password.
    pub fn finish_nextcloud(
        &self,
        label: &str,
        login: account_credentials::nextcloud::NextcloudLogin,
    ) -> Result<AccountId, ConnectionError> {
        self.password_account(
            accounts::cloud_types::NEXTCLOUD,
            label,
            &login.server,
            &login.login_name,
            login.app_password,
            None,
            true,
        )
    }

    /// Not a grant this installer minted, so cancellation must not revoke it.
    pub fn connect_nextcloud_app_password(
        &self,
        label: &str,
        login: account_credentials::nextcloud::NextcloudLogin,
    ) -> Result<AccountId, ConnectionError> {
        self.password_account(
            accounts::cloud_types::NEXTCLOUD,
            label,
            &login.server,
            &login.login_name,
            login.app_password,
            None,
            false,
        )
    }

    /// Supplied only through a private local buffer.
    pub fn connect_webdav(
        &self,
        label: &str,
        server: &str,
        username: &str,
        password: Zeroizing<String>,
    ) -> Result<AccountId, ConnectionError> {
        self.password_account(
            accounts::cloud_types::WEBDAV,
            label,
            server,
            username,
            password,
            None,
            false,
        )
    }

    pub(super) fn password_account(
        &self,
        type_id: &str,
        label: &str,
        server: &str,
        username: &str,
        password: Zeroizing<String>,
        id: Option<AccountId>,
        newly_authorized: bool,
    ) -> Result<AccountId, ConnectionError> {
        let parsed = url::Url::parse(server).map_err(|_| ConnectionError::InvalidConfiguration)?;
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || password.is_empty()
            || username.len() > 256
            || (type_id == accounts::cloud_types::NEXTCLOUD && username.is_empty())
        {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let server = account_credentials::nextcloud::server_url(server)
            .map_err(|_| ConnectionError::InvalidConfiguration)?
            .to_string();
        let imported = id.is_some();
        let id = id.unwrap_or_else(|| AccountId::new(uuid::Uuid::new_v4().to_string()));
        let mut account = Account::new(id, type_id);
        account.display_name = label.chars().take(256).collect();
        account.config =
            serde_json::json!({"server_url": server, "username": username, "use_files": true});
        if newly_authorized {
            account.config["app_password"] = serde_json::Value::Bool(true);
        }
        if imported {
            let secret = Secret::password(password.to_string());
            let copied = account_credentials::copied_secret(&account, &secret);
            self.persist_import(account, copied)
        } else {
            // Preserve offline copied grants: only an explicit new connection promises this endpoint
            // is ready before target ownership is published.
            let _ = self.accounts()?.state()?;
            super::dav_readiness::verify(&account, username, &password)?;
            self.persist(account, Secret::password(password.to_string()))
        }
    }

    /// Revoking a grant is best effort through existing ownership.
    pub fn disconnect(&self, id: &AccountId) -> Result<(), ConnectionError> {
        self.remove_prepared(id, true)
    }

    /// Forgetting a copied credential does not revoke the original provider grant, which the source
    /// device may still use.
    pub fn forget_prepared(&self, id: &AccountId) -> Result<(), ConnectionError> {
        self.remove_prepared(id, false)
    }

    fn remove_prepared(&self, id: &AccountId, revoke: bool) -> Result<(), ConnectionError> {
        let account = self.accounts()?.get(id.as_str())?;
        let mut vault = SecretVault::open_in(&self.root);
        if vault.recovery_error().is_some() {
            return Err(ConnectionError::StorageUnavailable);
        }
        let secret = vault.get(id).map(PrivateSecret);
        if revoke && let (Some(account), Some(secret)) = (&account, &secret) {
            self.credential_store.revoke(account, &secret.0);
        }
        // A retry can see the vault save of an earlier attempt whose metadata save failed; finish
        // removing it.
        vault.remove(id);
        vault
            .try_save()
            .map_err(|_| ConnectionError::StorageUnavailable)?;
        self.accounts()?.remove(id.as_str())?;
        self.accounts()?.project(&self.root)
    }

    /// Reseals one configured account with the destination vault's key. A live Secret Service or
    /// DPAPI source must be exported by its owner session, not copied as a key file. Collaborative
    /// accounts from other people are never adopted.
    pub fn import_tilo(&self, source: &Path, id: &AccountId) -> Result<AccountId, ConnectionError> {
        self.import_tilo_inner(source, id, None, ImportPurpose::CopySource)
    }

    pub fn import_tilo_reviewed(
        &self,
        source: &Path,
        reviewed: &super::TiloCandidate,
    ) -> Result<AccountId, ConnectionError> {
        self.import_tilo_inner(
            source,
            &AccountId::new(&reviewed.account_id),
            Some(reviewed),
            ImportPurpose::CopySource,
        )
    }

    /// Only the owned installer stage uses this; importing a source uses `import_tilo`, which keeps
    /// durable non-revoking provenance.
    pub fn handoff_prepared(
        &self,
        source: &Path,
        id: &AccountId,
    ) -> Result<AccountId, ConnectionError> {
        self.import_tilo_inner(source, id, None, ImportPurpose::PreparedHandoff)
    }

    fn import_tilo_inner(
        &self,
        source: &Path,
        id: &AccountId,
        reviewed: Option<&super::TiloCandidate>,
        purpose: ImportPurpose,
    ) -> Result<AccountId, ConnectionError> {
        if source == self.root {
            return Err(ConnectionError::AlreadyPresent);
        }
        let source_pin = crate::migration_directory::PinnedDirectory::open_absolute(source)
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let source_accounts = Accounts::read(source)?;
        let account = source_accounts
            .get(id.as_str())?
            .filter(|account| account.owner.is_none())
            .ok_or(ConnectionError::AccountMissing)?;
        if let Some(reviewed) = reviewed {
            if account.type_id.as_str() != reviewed.type_id
                || !account.enabled
                || super::tilo_sources::fingerprint(&account)? != reviewed.metadata_sha256
            {
                return Err(ConnectionError::AccountMissing);
            }
        }
        let vault_guard = crate::migration_directory::PinnedDirectory::open_absolute(
            &accounts::vault_dir_in(source),
        )
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
        source_pin
            .validate()
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let vault = SecretVault::open_read_only_in(source);
        vault_guard
            .validate()
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        source_pin
            .validate()
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        if vault.recovery_error().is_some() {
            return Err(ConnectionError::CredentialUnavailable);
        }
        let secret = PrivateSecret(
            vault
                .get(id)
                .filter(|secret| !secret.is_empty())
                .ok_or(ConnectionError::CredentialUnavailable)?,
        );
        if let Some(reviewed) = reviewed {
            let current = Accounts::read(source)?;
            let current = current
                .get(id.as_str())?
                .ok_or(ConnectionError::AccountMissing)?;
            if super::tilo_sources::fingerprint(&current)? != reviewed.metadata_sha256 {
                return Err(ConnectionError::AccountMissing);
            }
        }
        let expected = match purpose {
            ImportPurpose::CopySource => account_credentials::copied_secret(&account, &secret.0),
            ImportPurpose::PreparedHandoff => secret.0.clone(),
        };
        self.persist_import(account, expected)
    }

    /// Re-reads the consented source and matches its section, endpoint and login before extracting
    /// the grant; supported OAuth grants keep their original client registration.
    pub fn import_rclone(
        &self,
        roots: &super::SourceRoots,
        selected: &super::Candidate,
    ) -> Result<AccountId, ConnectionError> {
        if selected.origin != super::Origin::Rclone
            || !matches!(
                selected.provider,
                super::Provider::WebDav | super::Provider::Nextcloud
            )
            || selected.profile != roots.config.join("rclone/rclone.conf")
        {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let text = super::discovery::metadata(&roots.config, &selected.profile)
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .ok_or(ConnectionError::AccountMissing)?;
        let mut inventory = super::Inventory::default();
        super::discovery::rclone(&text, &selected.profile, &mut inventory);
        let current = inventory
            .candidates
            .into_iter()
            .find(|candidate| {
                candidate.origin == selected.origin
                    && candidate.provider == selected.provider
                    && candidate.label == selected.label
                    && candidate.server == selected.server
                    && candidate.identity == selected.identity
                    && candidate.profile == selected.profile
            })
            .ok_or(ConnectionError::AccountMissing)?;
        let mut section = "";
        let mut encoded = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = name;
            } else if section == current.label
                && let Some((key, value)) = line.split_once('=')
                && key.trim() == "pass"
            {
                if encoded.is_some() {
                    return Err(ConnectionError::CredentialUnavailable);
                }
                encoded = Some(value.trim());
            }
        }
        let password = reveal_rclone(encoded.ok_or(ConnectionError::CredentialUnavailable)?)?;
        // Rclone's WebDAV URL is the precise files root (including Nextcloud's
        // /remote.php/dav/files/<id> or a chosen subfolder), so keep it as generic DAV.
        let type_id = accounts::cloud_types::WEBDAV;
        self.password_account(
            type_id,
            &current.label,
            current
                .server
                .as_deref()
                .ok_or(ConnectionError::InvalidConfiguration)?,
            current.identity.as_deref().unwrap_or_default(),
            password,
            Some(rclone_id(&current)),
            false,
        )
    }

    pub(super) fn persist_import(
        &self,
        account: Account,
        secret: Secret,
    ) -> Result<AccountId, ConnectionError> {
        let id = account.id.clone();
        let kept = account.clone();
        let record = self.accounts()?.state()?;
        let _edits = crate::account_store::persist_import(&self.root, &record, account, secret)?;
        self.accounts()?.keep(&kept)?;
        self.accounts()?.project(&self.root)?;
        Ok(id)
    }

    pub(super) fn persist(
        &self,
        account: Account,
        secret: Secret,
    ) -> Result<AccountId, ConnectionError> {
        let id = account.id.clone();
        let kept = account.clone();
        let record = self.accounts()?.state()?;
        let _edits = crate::account_store::persist(&self.root, &record, account, secret)?;
        self.accounts()?.keep(&kept)?;
        self.accounts()?.project(&self.root)?;
        Ok(id)
    }
}

pub(super) fn credential_error(error: CredentialError) -> ConnectionError {
    match error {
        CredentialError::NotConfigured { .. } => ConnectionError::SignInUnavailable,
        CredentialError::Storage(_) => ConnectionError::StorageUnavailable,
        _ => ConnectionError::SignInFailed,
    }
}

// Rclone's obscure format: random 16-byte IV, AES-256 CTR, raw base64. Obfuscation, not ownership;
// reseal immediately.
fn reveal_rclone(encoded: &str) -> Result<Zeroizing<String>, ConnectionError> {
    if encoded.len() > 16 * 1024 {
        return Err(ConnectionError::CredentialUnavailable);
    }
    tilo_seal::compat::rclone_obscure(encoded).map_err(|_| ConnectionError::CredentialUnavailable)
}
pub(super) fn rclone_id(candidate: &super::Candidate) -> AccountId {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for bytes in [
        candidate.profile.as_os_str().as_encoded_bytes(),
        candidate.label.as_bytes(),
        candidate.server.as_deref().unwrap_or_default().as_bytes(),
        candidate.identity.as_deref().unwrap_or_default().as_bytes(),
    ] {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    AccountId::new(format!(
        "migrated-rclone-{}",
        hex::encode(digest.finalize())
    ))
}
