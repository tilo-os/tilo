//! The custodian: the one place that turns a sealed long-lived grant into a
//! short-lived credential, for the storage daemon (through the handoff
//! store) and for Mail (an access token for `XOAUTH2`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use accounts::{Account, AccountId, HandoffCredential, HandoffStore, Secret, SecretVault};
use serde::{Deserialize, Serialize};
use tilo_oauth::client::{Authorization, OAuthClient};
use tilo_oauth::{OAuthError, Provider, Registration, TokenSet};
use zeroize::Zeroizing;

use crate::clients::ClientTable;
use crate::icloud::{ICloudAuth, ICloudAuthError, ICloudSessionState, UnavailableICloud};
use crate::providers::{
    ICLOUD_DRIVE_TYPE, NEXTCLOUD_TYPE, OAuthCloud, OAuthIssuer, OAuthUse, WEBDAV_TYPE,
};

/// How long a browser sign-in may take.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Why custody could not produce a credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CustodyError {
    /// No client id is configured for this provider in this build.
    NotConfigured {
        provider: &'static str,
        key: &'static str,
    },
    /// The account must be signed in again interactively.
    NeedsAuthentication,
    /// The provider could not be reached.
    Offline,
    /// The account or its credential is missing or of another kind.
    Account(String),
    /// The vault or handoff store failed.
    Storage(String),
    /// A sign-in step failed.
    SignIn(String),
}

impl std::fmt::Display for CustodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured { provider, key } => write!(
                f,
                "{provider} sign-in is not configured in this build (set {key})"
            ),
            Self::NeedsAuthentication => f.write_str("sign in again in Settings"),
            Self::Offline => f.write_str("the provider could not be reached"),
            Self::Account(message) | Self::Storage(message) | Self::SignIn(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for CustodyError {}

impl From<OAuthError> for CustodyError {
    fn from(error: OAuthError) -> Self {
        match error {
            OAuthError::InvalidGrant => Self::NeedsAuthentication,
            OAuthError::Network(_) => Self::Offline,
            other => Self::SignIn(other.to_string()),
        }
    }
}

/// What custody holds for one `OAuth` grant, without releasing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantState {
    /// Nothing is sealed under this id on this device: never signed in
    /// here (a setting that roamed from another device), or forgotten.
    Missing,
    /// A grant custody can refresh.
    Usable,
    /// The provider revoked the grant, or it was sealed without offline
    /// access: the person must sign in again.
    SignInAgain,
}

/// The long-lived grant sealed in the vault's `token` field. Never handed
/// out of this crate except to be sealed.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Grant {
    Oauth2 { tokens: TokenSet },
    Icloud { apple_id: String, session: String },
}

/// Seal an `OAuth` sign-in result into a vault secret.
pub fn oauth_secret(tokens: &TokenSet) -> Secret {
    Secret {
        password: String::new(),
        token: serde_json::to_string(&Grant::Oauth2 {
            tokens: tokens.clone(),
        })
        .ok(),
    }
}

/// Seal an `iCloud` session into a vault secret. The Apple ID password is
/// not part of it.
pub fn icloud_secret(apple_id: &str, session: &ICloudSessionState) -> Secret {
    Secret {
        password: String::new(),
        token: serde_json::to_string(&Grant::Icloud {
            apple_id: apple_id.to_string(),
            session: session.0.to_string(),
        })
        .ok(),
    }
}

/// A sealed grant, and whether it is in the format Mail sealed before its
/// grants moved into custody: the bare [`TokenSet`] JSON, without the
/// `kind` tag. Such a grant reads as [`Grant::Oauth2`] and is resealed in
/// the current format on first use.
fn decode_grant(secret: &Secret) -> Option<(Grant, bool)> {
    let raw = secret.token.as_deref()?;
    if let Ok(grant) = serde_json::from_str(raw) {
        return Some((grant, false));
    }
    serde_json::from_str::<TokenSet>(raw)
        .ok()
        .map(|tokens| (Grant::Oauth2 { tokens }, true))
}

fn grant_of(secret: &Secret) -> Option<Grant> {
    decode_grant(secret).map(|(grant, _)| grant)
}

/// The Apple ID of a stored `iCloud` account, for the re-authentication form.
pub fn icloud_apple_id(secret: &Secret) -> Option<String> {
    match grant_of(secret)? {
        Grant::Icloud { apple_id, .. } => Some(apple_id),
        Grant::Oauth2 { .. } => None,
    }
}

/// Whether an account type's credential is held by this custodian (rather
/// than being a password another app uses directly).
pub fn is_cloud_type(type_id: &str) -> bool {
    OAuthCloud::from_type_id(type_id).is_some()
        || matches!(type_id, ICLOUD_DRIVE_TYPE | NEXTCLOUD_TYPE | WEBDAV_TYPE)
}

/// See the module docs.
#[derive(Clone)]
pub struct Custodian {
    config_root: Option<PathBuf>,
    icloud: Arc<dyn ICloudAuth>,
    clients: Arc<ClientTable>,
    overrides: BTreeMap<OAuthUse, (Provider, Registration)>,
    endpoints: BTreeMap<OAuthIssuer, Provider>,
    storage: BTreeMap<&'static str, serde_json::Value>,
    gmail_profile_endpoint: String,
}

impl Custodian {
    /// The signed-in user's stores.
    pub fn live() -> Self {
        Self::for_root(None)
    }

    /// The stores under `config_root` when given, else the user's.
    pub fn for_root(config_root: Option<PathBuf>) -> Self {
        Self {
            config_root,
            icloud: Arc::new(UnavailableICloud),
            clients: Arc::new(ClientTable::defaults().clone()),
            overrides: BTreeMap::new(),
            endpoints: BTreeMap::new(),
            storage: BTreeMap::new(),
            gmail_profile_endpoint: "https://gmail.googleapis.com/gmail/v1/users/me/profile".into(),
        }
    }

    /// Use `auth` for `iCloud` Drive.
    #[must_use]
    pub fn with_icloud(mut self, auth: Arc<dyn ICloudAuth>) -> Self {
        self.icloud = auth;
        self
    }

    /// Replace a provider's descriptor and registration (a client id, or a
    /// whole [`Registration`]) for tests and private deployments.
    #[must_use]
    pub fn with_provider(
        mut self,
        cloud: OAuthCloud,
        provider: Provider,
        registration: impl Into<Registration>,
    ) -> Self {
        self.overrides
            .insert(cloud.oauth_use(), (provider, registration.into()));
        self
    }

    /// Send every request for `issuer` to the endpoints of `provider` (a
    /// fake authorization server) while the scopes and the registration
    /// still come from the client table, as they do in production.
    #[must_use]
    pub fn with_issuer_endpoints(mut self, issuer: OAuthIssuer, provider: Provider) -> Self {
        self.endpoints.insert(issuer, provider);
        self
    }

    /// Use `table` instead of the committed client registrations
    /// (`oauth-clients.toml`); environment overrides still apply.
    #[must_use]
    pub fn with_client_table(mut self, table: ClientTable) -> Self {
        self.clients = Arc::new(table);
        self
    }

    /// Start from `config` (the provider crate's serde configuration: a
    /// fake server's API origin, a chosen folder) instead of the provider's
    /// default when resolving a new account's storage.
    #[must_use]
    pub fn with_storage_config(mut self, cloud: OAuthCloud, config: serde_json::Value) -> Self {
        self.storage.insert(cloud.type_id(), config);
        self
    }

    /// The non-secret mount configuration of a newly signed-in account,
    /// asked of the provider once with its fresh access token.
    ///
    /// # Errors
    /// See [`crate::storage::resolve`].
    pub fn resolve_storage(
        &self,
        cloud: OAuthCloud,
        tokens: &TokenSet,
    ) -> Result<serde_json::Value, CustodyError> {
        if cloud == OAuthCloud::GoogleDrive
            && let Some(granted) = &tokens.scope
        {
            let (provider, _) = self.oauth_config(cloud)?;
            if provider
                .scopes
                .iter()
                .any(|required| !granted.split_whitespace().any(|scope| scope == required))
            {
                return Err(CustodyError::SignIn(
                    "Google Drive permission was not granted".into(),
                ));
            }
        }
        crate::storage::resolve(
            cloud,
            self.storage.get(cloud.type_id()),
            &tokens.access_token,
        )
    }

    /// Use Apple's `iCloud` web sign-in ([`crate::icloud::WebICloud`]) for
    /// `iCloud` Drive. Settings and the credential helper run with it.
    #[must_use]
    pub fn with_apple_icloud(self) -> Self {
        match crate::icloud::WebICloud::apple() {
            Ok(auth) => self.with_icloud(Arc::new(auth)),
            Err(error) => {
                tracing::warn!(%error, "iCloud sign-in is unavailable");
                self
            }
        }
    }

    pub fn icloud(&self) -> &Arc<dyn ICloudAuth> {
        &self.icloud
    }

    pub fn config_root(&self) -> Option<&Path> {
        self.config_root.as_deref()
    }

    fn vault(&self) -> SecretVault {
        match &self.config_root {
            Some(root) => SecretVault::open_in(root),
            None => SecretVault::open(),
        }
    }

    fn store(&self) -> accounts::AccountStore {
        match &self.config_root {
            Some(root) => accounts::AccountStore::load_in(root),
            None => accounts::AccountStore::load(),
        }
    }

    /// The handoff store the daemon reads.
    ///
    /// # Errors
    /// [`CustodyError::Storage`] when no config directory resolves.
    pub fn handoff(&self) -> Result<HandoffStore, CustodyError> {
        match &self.config_root {
            Some(root) => Ok(HandoffStore::open_in(root)),
            None => HandoffStore::open()
                .ok_or_else(|| CustodyError::Storage("no config directory".into())),
        }
    }

    fn reporter(&self, id: &AccountId) -> accounts::AccountSyncReporter {
        match &self.config_root {
            Some(root) => accounts::AccountSyncReporter::in_root(id.clone(), root.clone()),
            None => accounts::AccountSyncReporter::new(id.clone()),
        }
    }

    /// The descriptor and registration for a storage provider, or the "not
    /// configured" state.
    ///
    /// # Errors
    /// [`CustodyError::NotConfigured`].
    pub fn oauth_config(
        &self,
        cloud: OAuthCloud,
    ) -> Result<(Provider, Registration), CustodyError> {
        self.use_config(cloud.oauth_use())
    }

    /// The descriptor (the issuer's endpoints with the scopes of this use)
    /// and the issuer's registration, or the "not configured" state.
    ///
    /// # Errors
    /// [`CustodyError::NotConfigured`] when the issuer has no client id or
    /// the table gives this use no scopes.
    pub fn use_config(
        &self,
        oauth_use: OAuthUse,
    ) -> Result<(Provider, Registration), CustodyError> {
        if let Some(configured) = self.overrides.get(&oauth_use) {
            return Ok(configured.clone());
        }
        let (descriptor, registration) =
            self.clients
                .config(oauth_use)
                .ok_or_else(|| CustodyError::NotConfigured {
                    provider: oauth_use.label(),
                    key: oauth_use.issuer.client_id().key,
                })?;
        let descriptor = match self.endpoints.get(&oauth_use.issuer) {
            Some(endpoints) => Provider {
                scopes: descriptor.scopes,
                ..endpoints.clone()
            },
            None => descriptor,
        };
        Ok((descriptor, registration))
    }

    /// Whether `oauth_use` can sign in in this build.
    pub fn is_configured(&self, oauth_use: OAuthUse) -> bool {
        self.use_config(oauth_use).is_ok()
    }

    /// Start a browser sign-in. Non-blocking: open
    /// [`Authorization::url`] with the desktop's URI opener, then pass the
    /// value to [`Self::finish_sign_in`] on a worker.
    ///
    /// # Errors
    /// [`CustodyError::NotConfigured`] or a loopback failure.
    pub fn begin_sign_in(
        &self,
        cloud: OAuthCloud,
        login_hint: Option<&str>,
    ) -> Result<Authorization, CustodyError> {
        self.begin_sign_in_for(cloud.oauth_use(), login_hint)
    }

    /// Start a browser sign-in for any use (Gmail, Outlook, a storage
    /// provider); see [`Self::begin_sign_in`].
    ///
    /// # Errors
    /// [`CustodyError::NotConfigured`] or a loopback failure.
    pub fn begin_sign_in_for(
        &self,
        oauth_use: OAuthUse,
        login_hint: Option<&str>,
    ) -> Result<Authorization, CustodyError> {
        let (provider, registration) = self.use_config(oauth_use)?;
        Authorization::begin(&provider, &registration, login_hint).map_err(CustodyError::from)
    }

    /// Start Google's files sign-in, optionally with explicitly requested Gmail
    /// access. Both capabilities must be configured; never silently drop mail.
    ///
    /// # Errors
    /// Unconfigured capability or a loopback failure.
    pub fn begin_google_sign_in(&self, mail: bool) -> Result<Authorization, CustodyError> {
        self.begin_google_services_sign_in(true, mail)
    }

    /// Start Google sign-in for files, mail, or both. Only selected services'
    /// configured scopes are requested; an unselected service need not be configured.
    /// Complete with [`Self::finish_sign_in`], then validate each selected service
    /// with [`Self::resolve_storage`] or [`Self::gmail_identity`] before saving it.
    ///
    /// # Errors
    /// No selected service, an unconfigured selected service, or a loopback failure.
    pub fn begin_google_services_sign_in(
        &self,
        files: bool,
        mail: bool,
    ) -> Result<Authorization, CustodyError> {
        let first = match (files, mail) {
            (true, _) => OAuthCloud::GoogleDrive.oauth_use(),
            (false, true) => OAuthUse::mail(OAuthIssuer::Google),
            (false, false) => {
                return Err(CustodyError::SignIn(
                    "select at least one Google service".into(),
                ));
            }
        };
        let (mut provider, registration) = self.use_config(first)?;
        if files && mail {
            let (gmail, _) = self.use_config(OAuthUse::mail(OAuthIssuer::Google))?;
            for scope in gmail.scopes {
                if !provider.scopes.contains(&scope) {
                    provider.scopes.push(scope);
                }
            }
        }
        Authorization::begin(&provider, &registration, None).map_err(CustodyError::from)
    }

    /// Replace the Gmail profile API origin for a local provider fixture.
    #[must_use]
    pub fn with_gmail_profile_endpoint(mut self, endpoint: String) -> Self {
        self.gmail_profile_endpoint = endpoint;
        self
    }

    /// Validate Gmail access and obtain the provider-confirmed mailbox identity.
    /// A Drive grant alone must never create a working Mail account.
    ///
    /// # Errors
    /// Missing mail permission, unavailable Gmail, or an invalid provider response.
    pub fn gmail_identity(&self, tokens: &TokenSet) -> Result<String, CustodyError> {
        if tokens.scope.as_deref().is_some_and(|scopes| {
            !scopes
                .split_whitespace()
                .any(|scope| scope == "https://mail.google.com/")
        }) {
            return Err(CustodyError::SignIn(
                "Gmail permission was not granted".into(),
            ));
        }
        let http = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CustodyError::Offline)?;
        let response = http
            .get(&self.gmail_profile_endpoint)
            .bearer_auth(&tokens.access_token)
            .send()
            .map_err(|_| CustodyError::Offline)?;
        if !response.status().is_success() {
            return Err(CustodyError::SignIn(format!(
                "Gmail could not be opened (HTTP {}). Check Gmail availability and mail permission.",
                response.status().as_u16()
            )));
        }
        let profile: serde_json::Value = response
            .json()
            .map_err(|_| CustodyError::SignIn("Gmail returned an invalid profile".into()))?;
        let email = profile
            .get("emailAddress")
            .and_then(serde_json::Value::as_str)
            .filter(|email| email.contains('@') && !email.contains(char::is_whitespace))
            .ok_or_else(|| CustodyError::SignIn("Gmail returned no mailbox address".into()))?;
        Ok(email.to_string())
    }

    /// Begin sign-in with a non-empty subset of this use's configured scopes.
    /// Client registration, endpoints and PKCE validation remain unchanged.
    ///
    /// # Errors
    /// Unconfigured use, an unconfigured scope, or a loopback failure.
    pub fn begin_sign_in_for_scopes(
        &self,
        oauth_use: OAuthUse,
        scopes: &[&str],
        login_hint: Option<&str>,
    ) -> Result<Authorization, CustodyError> {
        let (mut provider, registration) = self.use_config(oauth_use)?;
        if scopes.is_empty()
            || scopes
                .iter()
                .any(|scope| !provider.scopes.iter().any(|allowed| allowed == scope))
        {
            return Err(CustodyError::SignIn(
                "Requested scopes are not configured for this use".into(),
            ));
        }
        provider.scopes = scopes.iter().map(|scope| (*scope).to_string()).collect();
        Authorization::begin(&provider, &registration, login_hint).map_err(CustodyError::from)
    }

    /// Provider-confirmed scope metadata, without releasing credentials.
    /// `None` means the provider or an older grant did not supply metadata.
    ///
    /// # Errors
    /// Missing grant or an unreadable vault.
    pub fn oauth_scopes(&self, id: &AccountId) -> Result<Option<String>, CustodyError> {
        let vault = self.vault();
        if let Some(error) = vault.recovery_error() {
            return Err(CustodyError::Storage(error.to_string()));
        }
        let secret = vault.get(id).ok_or(CustodyError::NeedsAuthentication)?;
        let Some((Grant::Oauth2 { tokens }, _)) = decode_grant(&secret) else {
            return Err(CustodyError::NeedsAuthentication);
        };
        Ok(tokens.scope)
    }

    /// Whether `id`'s `OAuth` grant is there and still refreshable, for a
    /// "Sign in again" status. A grant the provider revoked is kept as an
    /// empty one when a refresh learns of it (see
    /// [`Self::oauth_access_token`]), which reads here as
    /// [`GrantState::SignInAgain`].
    ///
    /// # Errors
    /// An unreadable vault.
    pub fn oauth_grant_state(&self, id: &AccountId) -> Result<GrantState, CustodyError> {
        let vault = self.vault();
        if let Some(error) = vault.recovery_error() {
            return Err(CustodyError::Storage(error.to_string()));
        }
        let Some(secret) = vault.get(id) else {
            return Ok(GrantState::Missing);
        };
        Ok(match decode_grant(&secret) {
            Some((Grant::Oauth2 { tokens }, _)) if !tokens.refresh_token.is_empty() => {
                GrantState::Usable
            }
            _ => GrantState::SignInAgain,
        })
    }

    /// Seal a sign-in result as `id`'s grant, replacing any earlier one.
    ///
    /// # Errors
    /// [`CustodyError::Storage`] when the vault cannot be written.
    pub fn seal_oauth(&self, id: &AccountId, tokens: &TokenSet) -> Result<(), CustodyError> {
        let mut vault = self.vault();
        if let Some(error) = vault.recovery_error() {
            return Err(CustodyError::Storage(error.to_string()));
        }
        vault.put(id, &oauth_secret(tokens));
        vault.try_save().map_err(CustodyError::Storage)
    }

    /// Forget a local OAuth grant. Provider-side revocation remains separate.
    ///
    /// # Errors
    /// A vault recovery or persistence failure.
    pub fn forget_oauth(&self, id: &AccountId) -> Result<(), CustodyError> {
        let mut vault = self.vault();
        if let Some(error) = vault.recovery_error() {
            return Err(CustodyError::Storage(error.to_string()));
        }
        vault.remove(id);
        vault.try_save().map_err(CustodyError::Storage)
    }

    /// A current access token for `id`'s `OAuth` grant, for an app that
    /// presents it itself (Mail's `XOAUTH2`). The grant is refreshed through
    /// `oauth_use`'s client when it is due (or `force`) and the result
    /// resealed; a grant in an earlier format is migrated on the way. The
    /// refresh token never leaves custody.
    ///
    /// # Errors
    /// [`CustodyError::NeedsAuthentication`] when there is no usable grant
    /// or the provider revoked it, [`CustodyError::Offline`], or
    /// [`CustodyError::NotConfigured`] when a due refresh has no client.
    pub fn oauth_access_token(
        &self,
        id: &AccountId,
        oauth_use: OAuthUse,
        force: bool,
    ) -> Result<Zeroizing<String>, CustodyError> {
        self.oauth_access_token_with_expiry(id, oauth_use, force)
            .map(|(token, _)| token)
    }

    /// Return the same approved bearer with its provider-confirmed expiry.
    /// Refresh and revocation checks remain owned by the custodian.
    ///
    /// # Errors
    /// See [`Self::oauth_access_token`].
    pub fn oauth_access_token_with_expiry(
        &self,
        id: &AccountId,
        oauth_use: OAuthUse,
        force: bool,
    ) -> Result<(Zeroizing<String>, Option<u64>), CustodyError> {
        let mut vault = self.vault();
        if let Some(error) = vault.recovery_error() {
            return Err(CustodyError::Storage(error.to_string()));
        }
        let tokens = self.fresh_oauth_tokens(&mut vault, id, oauth_use, force)?;
        let expires = tokens.expires_at();
        Ok((Zeroizing::new(tokens.access_token), expires))
    }

    /// `id`'s sealed `OAuth` tokens, refreshed when due (or `force`), with
    /// any change sealed back into `vault`.
    fn fresh_oauth_tokens(
        &self,
        vault: &mut SecretVault,
        id: &AccountId,
        oauth_use: OAuthUse,
        force: bool,
    ) -> Result<TokenSet, CustodyError> {
        let secret = vault.get(id).ok_or(CustodyError::NeedsAuthentication)?;
        let Some((Grant::Oauth2 { mut tokens }, legacy)) = decode_grant(&secret) else {
            return Err(CustodyError::NeedsAuthentication);
        };
        if legacy {
            vault.put(id, &oauth_secret(&tokens));
            vault.try_save().map_err(CustodyError::Storage)?;
        }
        if tokens.refresh_token.is_empty() {
            return Err(CustodyError::NeedsAuthentication);
        }
        if force || tokens.needs_refresh() {
            let (provider, registration) = self.use_config(oauth_use)?;
            let renewed = match OAuthClient::new()?.refresh(
                &provider,
                &registration,
                &tokens.refresh_token,
            ) {
                Ok(renewed) => renewed.stamp_now(),
                Err(OAuthError::InvalidGrant) => {
                    // The provider revoked the grant: its access token
                    // must not be handed out again either. Keep the
                    // account, drop the dead grant until the next sign-in.
                    vault.put(id, &oauth_secret(&TokenSet::default()));
                    vault.try_save().map_err(CustodyError::Storage)?;
                    return Err(CustodyError::NeedsAuthentication);
                }
                Err(error) => return Err(error.into()),
            };
            let mut renewed = renewed;
            if renewed.scope.is_none() {
                renewed.scope = tokens.scope;
            }
            tokens = renewed;
            vault.put(id, &oauth_secret(&tokens));
            vault.try_save().map_err(CustodyError::Storage)?;
        }
        Ok(tokens)
    }

    /// Wait for the browser and redeem the code. Blocking.
    ///
    /// # Errors
    /// See [`OAuthError`], mapped into [`CustodyError`].
    pub fn finish_sign_in(authorization: Authorization) -> Result<TokenSet, CustodyError> {
        let client = OAuthClient::new()?;
        let tokens = authorization.complete(&client, SIGN_IN_TIMEOUT)?;
        if tokens.refresh_token.is_empty() {
            return Err(CustodyError::SignIn(
                "the provider did not grant offline access; try again".into(),
            ));
        }
        Ok(tokens)
    }

    /// Redeem the code the provider displayed, for a sign-in that
    /// [`Authorization::needs_pasted_code`]. Blocking.
    ///
    /// # Errors
    /// See [`OAuthError`], mapped into [`CustodyError`].
    pub fn finish_sign_in_with_code(
        authorization: Authorization,
        code: &str,
    ) -> Result<TokenSet, CustodyError> {
        let client = OAuthClient::new()?;
        let tokens = authorization.complete_with_code(&client, code)?;
        if tokens.refresh_token.is_empty() {
            return Err(CustodyError::SignIn(
                "the provider did not grant offline access; try again".into(),
            ));
        }
        Ok(tokens)
    }

    /// After a sign-in was saved: clear a stale "sign in again" health
    /// state and publish the first short-lived credential.
    ///
    /// # Errors
    /// See [`Self::publish`].
    pub fn signed_in(&self, id: &AccountId) -> Result<(), CustodyError> {
        let _ = self.reporter(id).remove();
        self.publish(id, false)
    }

    /// Publish a short-lived credential for one account in the handoff
    /// store: refresh an `OAuth` access token when it is due (or `force`),
    /// derive `iCloud` session headers, or pass through the server's own
    /// Basic credential for `WebDAV` and Nextcloud. A grant the provider
    /// no longer accepts marks the account as needing sign-in and withdraws
    /// its handoff entry.
    ///
    /// # Errors
    /// See [`CustodyError`].
    pub fn publish(&self, id: &AccountId, force: bool) -> Result<(), CustodyError> {
        let result = self.publish_inner(id, force);
        if result == Err(CustodyError::NeedsAuthentication) {
            let _ = self.reporter(id).needs_authentication();
            if let Ok(handoff) = self.handoff() {
                let _ = handoff.withdraw(id);
            }
        }
        result
    }

    fn publish_inner(&self, id: &AccountId, force: bool) -> Result<(), CustodyError> {
        let account = self
            .store()
            .get(id)
            .cloned()
            .ok_or_else(|| CustodyError::Account("account no longer exists".into()))?;
        if !account.enabled {
            return Err(CustodyError::Account("account is disabled".into()));
        }
        let mut vault = self.vault();
        if let Some(error) = vault.recovery_error() {
            return Err(CustodyError::Storage(error.to_string()));
        }
        let secret = vault.get(id).ok_or(CustodyError::NeedsAuthentication)?;
        let credential = if let Some(cloud) = OAuthCloud::from_type_id(account.type_id.as_str()) {
            let tokens = self.fresh_oauth_tokens(&mut vault, id, cloud.oauth_use(), force)?;
            HandoffCredential::Bearer {
                token: Zeroizing::new(tokens.access_token.clone()),
                expires_unix: tokens.expires_at(),
            }
        } else if account.type_id.as_str() == ICLOUD_DRIVE_TYPE {
            let Some(Grant::Icloud { apple_id, session }) = grant_of(&secret) else {
                return Err(CustodyError::NeedsAuthentication);
            };
            let (renewed, headers) = self
                .icloud
                .resume(&ICloudSessionState(Zeroizing::new(session)))
                .map_err(|error| match error {
                    ICloudAuthError::NeedsReauthentication
                    | ICloudAuthError::InvalidCredentials => CustodyError::NeedsAuthentication,
                    ICloudAuthError::Offline => CustodyError::Offline,
                    other => CustodyError::SignIn(other.to_string()),
                })?;
            vault.put(id, &icloud_secret(&apple_id, &renewed));
            vault.try_save().map_err(CustodyError::Storage)?;
            HandoffCredential::Session {
                headers: headers.headers,
                expires_unix: headers.expires_unix,
            }
        } else if matches!(account.type_id.as_str(), WEBDAV_TYPE | NEXTCLOUD_TYPE) {
            if secret.password.is_empty() {
                return Err(CustodyError::NeedsAuthentication);
            }
            let username = account
                .config
                .get("username")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            HandoffCredential::Basic {
                username,
                password: Zeroizing::new(secret.password),
            }
        } else {
            return Err(CustodyError::Account(
                "this account type is not a file provider".into(),
            ));
        };
        self.handoff()?
            .publish(id, &credential)
            .map_err(CustodyError::Storage)
    }

    /// Give up an account's grant at its provider, best effort, after the
    /// account was removed locally. `secret` is the credential the removal
    /// took out of the vault.
    pub fn revoke(&self, account: &Account, secret: &Secret) {
        if let Ok(handoff) = self.handoff() {
            let _ = handoff.withdraw(&account.id);
        }
        let type_id = account.type_id.as_str();
        if let Some(cloud) = OAuthCloud::from_type_id(type_id) {
            let (Some(Grant::Oauth2 { tokens }), Ok((provider, registration))) =
                (grant_of(secret), self.oauth_config(cloud))
            else {
                return;
            };
            if tokens.access_token.is_empty() && tokens.refresh_token.is_empty() {
                return;
            }
            let result = OAuthClient::new()
                .and_then(|client| client.revoke(&provider, &registration, &tokens));
            if let Err(error) = result {
                tracing::warn!(provider = cloud.label(), %error, "could not revoke a removed account's grant");
            }
        } else if type_id == ICLOUD_DRIVE_TYPE {
            if let Some(Grant::Icloud { session, .. }) = grant_of(secret) {
                self.icloud
                    .sign_out(&ICloudSessionState(Zeroizing::new(session)));
            }
        } else if type_id == NEXTCLOUD_TYPE && !secret.password.is_empty() {
            let text = |key: &str| {
                account
                    .config
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            // Only app passwords minted by Login Flow v2 are revoked; an
            // older account may hold the person's own password.
            if account
                .config
                .get("app_password")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                && let Err(error) = crate::nextcloud::revoke_app_password(
                    &text("server_url"),
                    &text("username"),
                    &secret.password,
                )
            {
                tracing::warn!(%error, "could not revoke a removed Nextcloud app password");
            }
        }
    }
}

impl accounts::CredentialRefresher for Custodian {
    fn refresh(&self, id: &AccountId, force: bool) -> Result<(), accounts::RefreshError> {
        self.publish(id, force).map_err(|error| match error {
            CustodyError::NeedsAuthentication => accounts::RefreshError::NeedsAuthentication,
            CustodyError::Offline => accounts::RefreshError::Offline,
            _ => accounts::RefreshError::Failed,
        })
    }
}
