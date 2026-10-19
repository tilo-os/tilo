use std::path::{Path, PathBuf};

use account_custody::{Authorization, Custodian, CustodyError, OAuthCloud};
use accounts::{Account, AccountId, AccountStore, Secret, SecretVault};
use zeroize::Zeroizing;

pub use crate::account_store::ConnectionError;
pub(super) use crate::account_store::PrivateSecret;

#[derive(Clone, Copy)]
enum ImportPurpose {
    CopySource,
    PreparedHandoff,
}

/// A private destination configuration root, separate from shared UI state.
/// It may be staged for migration or be the target user's installed home.
#[derive(Clone)]
pub struct Connector {
    pub(super) root: PathBuf,
    pub(super) custodian: Custodian,
    #[cfg(any(test, feature = "test-support"))]
    pub(super) spotify_api: Option<String>,
    #[cfg(all(test, target_os = "linux"))]
    pub(super) goa_api: Option<String>,
}

impl Connector {
    pub fn new(root: PathBuf) -> Self {
        let custodian = Custodian::for_root(Some(root.clone())).with_apple_icloud();
        Self {
            root,
            custodian,
            #[cfg(any(test, feature = "test-support"))]
            spotify_api: None,
            #[cfg(all(test, target_os = "linux"))]
            goa_api: None,
        }
    }

    /// Injection uses the production custodian's provider overrides in tests.
    pub fn with_custodian(root: PathBuf, custodian: Custodian) -> Self {
        Self {
            root,
            custodian,
            #[cfg(any(test, feature = "test-support"))]
            spotify_api: None,
            #[cfg(all(test, target_os = "linux"))]
            goa_api: None,
        }
    }

    pub fn cloud_available(&self, cloud: OAuthCloud) -> bool {
        self.custodian.is_configured(cloud.oauth_use())
    }

    /// Only call after explicit connection consent. The caller opens this
    /// authorization's URL through the shared host URI opener; no app is
    /// launched by the backend. Keep the handle out of CRDT/actions/status.
    pub fn begin_oauth(
        &self,
        cloud: OAuthCloud,
        identity: Option<&str>,
    ) -> Result<Authorization, ConnectionError> {
        self.custodian
            .begin_sign_in(cloud, identity)
            .map_err(custody_error)
    }

    /// Run on a worker after the person approves the browser sign-in.
    /// PKCE, state validation and grant custody are the existing platform's.
    pub fn finish_oauth(
        &self,
        cloud: OAuthCloud,
        label: &str,
        authorization: Authorization,
    ) -> Result<AccountId, ConnectionError> {
        let tokens = Custodian::finish_sign_in(authorization).map_err(custody_error)?;
        self.persist_cloud_tokens(cloud, label, tokens)
    }

    pub fn finish_oauth_with_code(
        &self,
        cloud: OAuthCloud,
        label: &str,
        authorization: Authorization,
        code: Zeroizing<String>,
    ) -> Result<AccountId, ConnectionError> {
        let tokens =
            Custodian::finish_sign_in_with_code(authorization, &code).map_err(custody_error)?;
        self.persist_cloud_tokens(cloud, label, tokens)
    }

    fn persist_cloud_tokens(
        &self,
        cloud: OAuthCloud,
        label: &str,
        tokens: account_custody::TokenSet,
    ) -> Result<AccountId, ConnectionError> {
        let mount = self
            .custodian
            .resolve_storage(cloud, &tokens)
            .map_err(custody_error)?;
        let mut account = Account::new(
            AccountId::new(uuid::Uuid::new_v4().to_string()),
            cloud.type_id(),
        );
        account.display_name = label.chars().take(256).collect();
        account.config = serde_json::json!({"provider": cloud.type_id(), accounts::cloud_types::MOUNT_CONFIG_KEY: mount});
        self.persist(account, account_custody::oauth_secret(&tokens))
    }

    /// Spotify's read-only library connection, using the exact grant slot
    /// consumed by Tilo Music. Streaming/playback remains a separate consent.
    pub fn begin_spotify(&self) -> Result<Authorization, ConnectionError> {
        use account_custody::{Capability, OAuthIssuer, OAuthUse};
        self.custodian
            .begin_sign_in_for_scopes(
                OAuthUse::new(OAuthIssuer::Spotify, Capability::Music),
                &["user-read-private", "user-library-read"],
                None,
            )
            .map_err(custody_error)
    }

    pub fn finish_spotify(
        &self,
        authorization: Authorization,
    ) -> Result<AccountId, ConnectionError> {
        let id = AccountId::from("music.spotify");
        if load_store(&self.root)?.get(&id).is_some() {
            return Err(ConnectionError::AlreadyPresent);
        }
        let mut tokens = Custodian::finish_sign_in(authorization).map_err(custody_error)?;
        if tokens.scope.is_none() {
            tokens.scope = Some("user-read-private user-library-read".into());
        }
        self.persist_verified_spotify(id, tokens, false)
    }

    /// Separate playback consent for the exact slot consumed by native Music.
    pub fn begin_spotify_streaming(&self) -> Result<Authorization, ConnectionError> {
        use account_custody::{Capability, OAuthIssuer, OAuthUse};
        self.custodian
            .begin_sign_in_for_scopes(
                OAuthUse::new(OAuthIssuer::Spotify, Capability::MusicStreaming),
                &["streaming", "user-read-private"],
                None,
            )
            .map_err(custody_error)
    }

    pub fn finish_spotify_streaming(
        &self,
        authorization: Authorization,
    ) -> Result<AccountId, ConnectionError> {
        let id = AccountId::from("music.spotify.streaming");
        if load_store(&self.root)?.get(&id).is_some() {
            return Err(ConnectionError::AlreadyPresent);
        }
        let mut tokens = Custodian::finish_sign_in(authorization).map_err(custody_error)?;
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

    /// Existing Apple sign-in adapter; password/challenge/code stay private
    /// on the worker and never enter the install profile.
    pub fn icloud_available(&self) -> bool {
        self.custodian.icloud().is_available()
    }

    /// End only a newly granted session that could not enter canonical custody.
    pub fn icloud_sign_out(&self, session: &account_custody::icloud::ICloudSessionState) {
        self.custodian.icloud().sign_out(session);
    }

    pub fn icloud_sign_in(
        &self,
        apple_id: &str,
        password: Zeroizing<String>,
    ) -> Result<account_custody::icloud::ICloudSignIn, account_custody::icloud::ICloudAuthError>
    {
        self.custodian.icloud().sign_in(apple_id, &password)
    }

    pub fn icloud_verify(
        &self,
        challenge: &account_custody::icloud::ICloudChallenge,
        code: Zeroizing<String>,
    ) -> Result<account_custody::icloud::ICloudSessionState, account_custody::icloud::ICloudAuthError>
    {
        self.custodian.icloud().verify_code(challenge, &code)
    }

    pub fn finish_icloud(
        &self,
        apple_id: &str,
        session: account_custody::icloud::ICloudSessionState,
    ) -> Result<AccountId, ConnectionError> {
        if apple_id.trim().is_empty() || apple_id.len() > 256 {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let mut account = Account::new(
            AccountId::new(uuid::Uuid::new_v4().to_string()),
            accounts::cloud_types::ICLOUD_DRIVE,
        );
        account.display_name = apple_id.to_string();
        let mut config = serde_json::Map::new();
        config.insert("identity".into(), apple_id.into());
        let mount = self.custodian.icloud().storage_config();
        if !mount.is_null() {
            config.insert(accounts::cloud_types::MOUNT_CONFIG_KEY.into(), mount);
        }
        account.config = serde_json::Value::Object(config);
        self.persist(account, account_custody::icloud_secret(apple_id, &session))
    }

    /// Complete the existing Nextcloud Login Flow v2 (begin/wait are exposed
    /// by account_custody::nextcloud). The user's ordinary password never
    /// enters the installer; this is the device's revocable app password.
    pub fn finish_nextcloud(
        &self,
        label: &str,
        login: account_custody::nextcloud::NextcloudLogin,
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

    /// Retain an explicitly supplied existing Nextcloud app password. This is
    /// not a grant minted by this installer, so cancellation must not revoke it.
    pub fn connect_nextcloud_app_password(
        &self,
        label: &str,
        login: account_custody::nextcloud::NextcloudLogin,
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

    /// An explicitly supplied WebDAV app password, sealed in the vault.
    /// Passwords are supplied only through a private local buffer.
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
        let server = account_custody::nextcloud::server_url(server)
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
            let copied = account_custody::copied_secret(&account, &secret);
            self.persist_import(account, copied)
        } else {
            // Preserve offline copied grants. Only explicit new connections
            // promise this endpoint is ready before publishing target custody.
            let _ = load_store(&self.root)?;
            super::dav_readiness::verify(&account, username, &password)?;
            self.persist(account, Secret::password(password.to_string()))
        }
    }

    /// Remove a cancelled prepared connection from its private stage. Revoking
    /// a supported provider grant is best effort through existing custody.
    pub fn disconnect(&self, id: &AccountId) -> Result<(), ConnectionError> {
        self.remove_prepared(id, true)
    }

    /// Forget a copied credential in private staging without revoking its
    /// original provider grant, which may still be used on the source device.
    pub fn forget_prepared(&self, id: &AccountId) -> Result<(), ConnectionError> {
        self.remove_prepared(id, false)
    }

    fn remove_prepared(&self, id: &AccountId, revoke: bool) -> Result<(), ConnectionError> {
        let mut store = load_store(&self.root)?;
        let account = store.get(id).cloned();
        let mut vault = SecretVault::open_in(&self.root);
        if vault.recovery_error().is_some() {
            return Err(ConnectionError::StorageUnavailable);
        }
        let secret = vault.get(id).map(PrivateSecret);
        if revoke && let (Some(account), Some(secret)) = (&account, &secret) {
            self.custodian.revoke(account, &secret.0);
        }
        // A retry can observe the vault save from an earlier attempt whose
        // metadata save failed. Finish removing the metadata in that case.
        vault.remove(id);
        vault
            .try_save()
            .map_err(|_| ConnectionError::StorageUnavailable)?;
        store.remove(id);
        store
            .save_in(&self.root)
            .map_err(|_| ConnectionError::StorageUnavailable)
    }

    /// Reseal one already configured Tilo account with the destination vault's
    /// key. Source vaults with explicit-root custody are supported. A live
    /// Secret Service/DPAPI-only source must be exported by its owner session,
    /// not copied as a key file. Missing/unavailable credentials fail closed.
    /// Collaborative accounts contributed by other people are never adopted.
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

    /// Transfer already prepared private custody without changing its lifecycle.
    /// Only the owned installer stage uses this; importing an original source uses
    /// `import_tilo` and receives durable non-revoking copied provenance.
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
        let store = load_store(source)?;
        let account = store
            .get(id)
            .filter(|account| account.owner.is_none())
            .cloned()
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
            let current = load_store(source)?;
            let current = current.get(id).ok_or(ConnectionError::AccountMissing)?;
            if super::tilo_sources::fingerprint(current)? != reviewed.metadata_sha256 {
                return Err(ConnectionError::AccountMissing);
            }
        }
        let expected = match purpose {
            ImportPurpose::CopySource => account_custody::copied_secret(&account, &secret.0),
            ImportPurpose::PreparedHandoff => secret.0.clone(),
        };
        self.persist_import(account, expected)
    }

    /// Import one reviewed rclone WebDAV/Nextcloud account. Re-read the exact
    /// consented source and match its section, endpoint and login before
    /// extracting its reusable grant. Supported OAuth grants retain their original
    /// client registration. No source file is changed.
    pub fn import_rclone(
        &self,
        roots: &super::SourceRoots,
        selected: &super::Candidate,
    ) -> Result<AccountId, ConnectionError> {
        #[cfg(unix)]
        if matches!(
            selected.provider,
            super::Provider::Google | super::Provider::Microsoft | super::Provider::Dropbox
        ) {
            return self.import_rclone_oauth(roots, selected);
        }
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
        // Rclone's WebDAV URL is the precise remote files root (including
        // Nextcloud's /remote.php/dav/files/<id> or a chosen subfolder). Preserve
        // it as generic DAV; treating it as a Nextcloud server base would append
        // a second DAV path and can also confuse login name with DAV user id.
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
        crate::account_store::persist_import(&self.root, || load_store(&self.root), account, secret)
    }

    pub(super) fn persist(
        &self,
        account: Account,
        secret: Secret,
    ) -> Result<AccountId, ConnectionError> {
        crate::account_store::persist(&self.root, || load_store(&self.root), account, secret)
    }
}

pub(super) fn custody_error(error: CustodyError) -> ConnectionError {
    match error {
        CustodyError::NotConfigured { .. } => ConnectionError::SignInUnavailable,
        CustodyError::Storage(_) => ConnectionError::StorageUnavailable,
        _ => ConnectionError::SignInFailed,
    }
}

// Rclone's public obscure format: random 16-byte IV, AES-256 CTR, raw URL
// base64. This is obfuscation, not secure custody; reseal immediately.
// https://github.com/rclone/rclone/blob/master/fs/config/obscure/obscure.go
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

pub(super) fn load_store(root: &Path) -> Result<AccountStore, ConnectionError> {
    let path = accounts::store_path_in(root);
    let data =
        super::discovery::metadata(root, &path).map_err(|_| ConnectionError::StorageUnavailable)?;
    match data {
        None => Ok(AccountStore::new()),
        Some(data) => serde_json::from_str(&data).map_err(|_| ConnectionError::StorageUnavailable),
    }
}
