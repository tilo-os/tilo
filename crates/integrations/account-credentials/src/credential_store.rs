//! The credential store: the one place that turns a sealed long-lived grant into a
//! short-lived credential, for a drive that reads it (an access token or session
//! headers, made in memory: [`CredentialStore::drive_credential`]) and for Mail (an access token
//! for `XOAUTH2`). A `WebDAV` or Nextcloud password is no grant of this device's: it is the
//! workspace's vault entry's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use accounts::{Account, AccountId, Secret, SecretVault};
use serde::{Deserialize, Serialize};
use tilo_actions::remote::person_accounts::{CloudCredential, CloudCredentialKind, CloudHeader};
use tilo_oauth::client::{Authorization, OAuthClient};
use tilo_oauth::device::DeviceAuthorization;
use tilo_oauth::{OAuthError, Provider, Registration, TokenSet};
use zeroize::{Zeroize, Zeroizing};

use crate::clients::ClientTable;
use crate::icloud::{ICloudAuth, ICloudAuthError, ICloudSessionState, UnavailableICloud};
use crate::providers::{
    ICLOUD_DRIVE_TYPE, NEXTCLOUD_TYPE, OAuthCloud, OAuthIssuer, OAuthUse, WEBDAV_TYPE,
    valid_mailbox,
};

/// The protected grant wire protocol this loaded credential store implements. Native
/// runtime certification invokes this non-inlined symbol through the actual SDK
/// dylib, separately from the installed credential helper; it is not a package
/// version or a claim made by a prepared-account importer.
#[inline(never)]
pub fn protected_grant_protocol() -> u32 {
    1
}

/// How long a browser sign-in may take.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialError {
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
    SignIn(String),
}

impl std::fmt::Display for CredentialError {
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

impl std::error::Error for CredentialError {}

impl From<OAuthError> for CredentialError {
    fn from(error: OAuthError) -> Self {
        match error {
            OAuthError::InvalidGrant => Self::NeedsAuthentication,
            OAuthError::Network(_) => Self::Offline,
            other => Self::SignIn(other.to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantState {
    /// Nothing is sealed under this id on this device: never signed in here (a
    /// setting that roamed from another device), or forgotten.
    Missing,
    /// A grant ownership can refresh.
    Usable,
    /// The provider revoked the grant, or it was sealed without offline access: the
    /// person must sign in again.
    SignInAgain,
}

/// The long-lived grant sealed in the vault's `token` field; never handed out of
/// this crate except to be sealed.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Grant {
    #[serde(alias = "protected_oauth2")]
    Oauth2 {
        tokens: TokenSet,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        copied: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        copy_origin: Option<CopyOrigin>,
    },
    #[serde(alias = "copied_icloud")]
    Icloud {
        apple_id: String,
        session: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        copied: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        copy_origin: Option<CopyOrigin>,
    },
    #[serde(alias = "protected_password")]
    CopiedPassword {
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        copy_origin: Option<CopyOrigin>,
    },
}

// Protected kinds deliberately differ from the legacy decoder's known variants,
// and their credentials stay nested, so an older decoder cannot fall back to
// TokenSet or use an outer plaintext password after ignoring copy provenance.
impl Serialize for Grant {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        match self {
            Self::Oauth2 {
                tokens,
                copied,
                copy_origin,
            } => {
                let protected = *copied || copy_origin.is_some();
                map.serialize_entry(
                    "kind",
                    if protected {
                        "protected_oauth2"
                    } else {
                        "oauth2"
                    },
                )?;
                map.serialize_entry("tokens", tokens)?;
                if protected {
                    map.serialize_entry("copied", copied)?;
                    map.serialize_entry("copy_origin", copy_origin)?;
                }
            }
            Self::Icloud {
                apple_id,
                session,
                copied,
                copy_origin,
            } => {
                map.serialize_entry(
                    "kind",
                    if *copied || copy_origin.is_some() {
                        "copied_icloud"
                    } else {
                        "icloud"
                    },
                )?;
                map.serialize_entry("apple_id", apple_id)?;
                map.serialize_entry("session", session)?;
                if *copied || copy_origin.is_some() {
                    map.serialize_entry("copied", copied)?;
                    map.serialize_entry("copy_origin", copy_origin)?;
                }
            }
            Self::CopiedPassword {
                password,
                copy_origin,
            } => {
                map.serialize_entry(
                    "kind",
                    if password.is_some() {
                        "protected_password"
                    } else {
                        "copied_password"
                    },
                )?;
                if let Some(password) = password {
                    map.serialize_entry("password", password)?;
                }
                map.serialize_entry("copy_origin", copy_origin)?;
            }
        }
        map.end()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyOrigin {
    credential_sha256: [u8; 32],
    account_sha256: [u8; 32],
}

fn registered_secret(tokens: &TokenSet, copied: bool, copy_origin: Option<CopyOrigin>) -> Secret {
    Secret {
        password: String::new(),
        token: serde_json::to_string(&Grant::Oauth2 {
            tokens: tokens.clone(),
            copied,
            copy_origin,
        })
        .ok(),
    }
}

/// The tokens a sealed `OAuth` grant holds, when the secret is one: what a
/// reader of a person's grant reads without renewing it.
#[must_use]
pub fn oauth_tokens(secret: &Secret) -> Option<TokenSet> {
    match decode_grant(secret)?.0 {
        Grant::Oauth2 { tokens, .. } => Some(tokens),
        Grant::Icloud { .. } | Grant::CopiedPassword { .. } => None,
    }
}

/// Seal an `OAuth` sign-in result into a vault secret.
pub fn oauth_secret(tokens: &TokenSet) -> Secret {
    Secret {
        password: String::new(),
        token: Some(crate::oauth_grant(tokens)),
    }
}

/// Seal an `iCloud` session into a vault secret; the Apple ID password is not
/// part of it.
pub fn icloud_secret(apple_id: &str, session: &ICloudSessionState) -> Secret {
    icloud_secret_with_provenance(apple_id, session, false, None)
}

fn icloud_secret_with_provenance(
    apple_id: &str,
    session: &ICloudSessionState,
    copied: bool,
    copy_origin: Option<CopyOrigin>,
) -> Secret {
    Secret {
        password: String::new(),
        token: serde_json::to_string(&Grant::Icloud {
            apple_id: apple_id.into(),
            session: session.0.to_string(),
            copied,
            copy_origin,
        })
        .ok(),
    }
}

/// Copy canonical ownership without transferring authority to revoke the original
/// sign-in: the grant, original registration and password remain usable, and this
/// provenance is sealed and kept through refresh and portable resealing.
pub fn copied_secret(account: &Account, secret: &Secret) -> Secret {
    use sha2::{Digest, Sha256};
    let digest = |domain: &[u8], encoded: String| {
        let encoded = Zeroizing::new(encoded);
        let mut hash = Sha256::new();
        hash.update(domain);
        hash.update(encoded.as_bytes());
        <[u8; 32]>::from(hash.finalize())
    };
    let origin = CopyOrigin {
        credential_sha256: copy_origin(secret).map_or_else(
            || {
                digest(
                    b"tilo-account-copy-credential/v1\0",
                    serde_json::to_string(secret).unwrap_or_default(),
                )
            },
            |origin| origin.credential_sha256,
        ),
        account_sha256: digest(
            b"tilo-account-copy-owner-metadata/v1\0",
            serde_json::to_string(account).unwrap_or_default(),
        ),
    };
    let mut copy = secret.clone();
    let grant = match grant_of(secret) {
        Some(Grant::Oauth2 { tokens, .. }) => Some(Grant::Oauth2 {
            tokens,
            copied: true,
            copy_origin: Some(origin),
        }),
        Some(Grant::Icloud {
            apple_id, session, ..
        }) => Some(Grant::Icloud {
            apple_id,
            session,
            copied: true,
            copy_origin: Some(origin),
        }),
        Some(Grant::CopiedPassword { password, .. }) => Some(Grant::CopiedPassword {
            password,
            copy_origin: Some(origin),
        }),
        None if secret.token.is_none() && !secret.password.is_empty() => {
            Some(Grant::CopiedPassword {
                password: matches!(account.type_id.as_str(), WEBDAV_TYPE | NEXTCLOUD_TYPE)
                    .then(|| secret.password.clone()),
                copy_origin: Some(origin),
            })
        }
        None => None,
    };
    if let Some(grant) = grant {
        if matches!(
            grant,
            Grant::CopiedPassword {
                password: Some(_),
                ..
            }
        ) {
            copy.password.zeroize();
        }
        copy.token = serde_json::to_string(&grant).ok();
    }
    copy
}

fn copy_origin(secret: &Secret) -> Option<CopyOrigin> {
    match grant_of(secret)? {
        Grant::Oauth2 {
            copied: true,
            copy_origin,
            ..
        }
        | Grant::Icloud {
            copied: true,
            copy_origin,
            ..
        }
        | Grant::CopiedPassword { copy_origin, .. } => copy_origin,
        Grant::Oauth2 { .. } | Grant::Icloud { .. } => None,
    }
}

/// Match an explicitly copied source after its target access or refresh token
/// changed. Both the original credential and the owner metadata fingerprints must
/// match.
pub fn copied_source_matches(stored: &Secret, expected: &Secret) -> bool {
    let usable = match grant_of(stored) {
        Some(Grant::Oauth2 { tokens, .. }) => !tokens.refresh_token.is_empty(),
        Some(Grant::Icloud { session, .. }) => !session.is_empty(),
        Some(Grant::CopiedPassword { .. }) => {
            !password_credential(stored).is_empty()
                && password_credential(stored) == password_credential(expected)
                && stored.password == expected.password
        }
        None => false,
    };
    if !usable {
        return false;
    }
    match (copy_origin(stored), copy_origin(expected)) {
        (Some(stored), Some(expected)) => stored == expected,
        _ => false,
    }
}

/// Read a password through canonical ownership, including protected copied `DAV`
/// grants. Other account types keep their plain password representation.
pub fn password_credential(secret: &Secret) -> Zeroizing<String> {
    if let Some(Grant::CopiedPassword {
        password: Some(password),
        ..
    }) = grant_of(secret)
    {
        return Zeroizing::new(password);
    }
    if has_grant_tag(secret) && grant_of(secret).is_none() {
        return Zeroizing::new(String::new());
    }
    Zeroizing::new(secret.password.clone())
}

/// Whether preserving this grant's client and copy authority requires protocol 1.
pub fn requires_protected_consumer(secret: &Secret) -> bool {
    match grant_of(secret) {
        Some(
            Grant::Oauth2 {
                copied,
                copy_origin,
                ..
            }
            | Grant::Icloud {
                copied,
                copy_origin,
                ..
            },
        ) => copied || copy_origin.is_some(),
        Some(Grant::CopiedPassword { .. }) => true,
        None => false,
    }
}

/// Whether canonical removal should detach this copy without revoking its source.
pub fn is_copied_secret(secret: &Secret) -> bool {
    matches!(
        grant_of(secret),
        Some(
            Grant::Oauth2 { copied: true, .. }
                | Grant::Icloud { copied: true, .. }
                | Grant::CopiedPassword { .. }
        )
    )
}

/// A sealed grant, and whether it is in the format Mail sealed before its grants
/// moved into ownership: bare [`TokenSet`] JSON without the `kind` tag. Such a
/// grant reads as [`Grant::Oauth2`] and is resealed in the current format on
/// first use.
fn decode_grant(secret: &Secret) -> Option<(Grant, bool)> {
    let raw = secret.token.as_deref()?;
    if let Ok(grant) = serde_json::from_str(raw) {
        return Some((grant, false));
    }
    // Tagged objects are never legacy bare tokens, even with an access_token field;
    // refuse unknown future protected ownership formats.
    if has_grant_tag(secret) {
        return None;
    }
    serde_json::from_str::<TokenSet>(raw).ok().map(|tokens| {
        (
            Grant::Oauth2 {
                tokens,
                copied: false,
                copy_origin: None,
            },
            true,
        )
    })
}

fn has_grant_tag(secret: &Secret) -> bool {
    secret
        .token
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .is_some_and(|value| value.get("kind").is_some())
}

fn grant_of(secret: &Secret) -> Option<Grant> {
    decode_grant(secret).map(|(grant, _)| grant)
}

/// The Apple ID of a stored `iCloud` account, for the re-authentication form.
pub fn icloud_apple_id(secret: &Secret) -> Option<String> {
    match grant_of(secret)? {
        Grant::Icloud { apple_id, .. } => Some(apple_id),
        Grant::Oauth2 { .. } | Grant::CopiedPassword { .. } => None,
    }
}

/// Whether an account type's credential is held by this credential store rather than
/// being a password another app uses directly.
pub fn is_cloud_type(type_id: &str) -> bool {
    OAuthCloud::from_type_id(type_id).is_some()
        || matches!(type_id, ICLOUD_DRIVE_TYPE | NEXTCLOUD_TYPE | WEBDAV_TYPE)
}

/// A grant read and, where it needed it, renewed ([`CredentialStore::renew_grant`]).
pub struct RenewedGrant {
    /// The tokens to sign in with now: none where the provider will not renew the grant any
    /// more, and its person signs in again.
    pub tokens: Option<TokenSet>,
    /// What to keep in place of the secret read, where that changed: the renewed grant, or the
    /// emptied one a provider would not renew.
    pub keep: Option<Secret>,
}

impl std::fmt::Debug for RenewedGrant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RenewedGrant")
            .field("signs_in", &self.tokens.is_some())
            .field("changed", &self.keep.is_some())
            .finish()
    }
}

/// See the module docs.
#[derive(Clone)]
pub struct CredentialStore {
    config_root: Option<PathBuf>,
    icloud: Arc<dyn ICloudAuth>,
    clients: Arc<ClientTable>,
    overrides: BTreeMap<OAuthUse, (Provider, Registration)>,
    endpoints: BTreeMap<OAuthIssuer, Provider>,
    storage: BTreeMap<&'static str, serde_json::Value>,
    gmail_profile_endpoint: String,
    microsoft_profile_endpoint: String,
}

impl CredentialStore {
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
            microsoft_profile_endpoint: "https://graph.microsoft.com/v1.0/me?$select=mail".into(),
        }
    }

    #[must_use]
    pub fn with_icloud(mut self, auth: Arc<dyn ICloudAuth>) -> Self {
        self.icloud = auth;
        self
    }

    /// Replace a provider's descriptor and registration (a client id, or a whole
    /// [`Registration`]) for tests and private deployments.
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

    /// Sign in for `oauth_use` with `provider` and `registration` (a fake
    /// authorization server's) instead of the client table's.
    #[must_use]
    pub fn with_use(
        mut self,
        oauth_use: OAuthUse,
        provider: Provider,
        registration: impl Into<Registration>,
    ) -> Self {
        self.overrides
            .insert(oauth_use, (provider, registration.into()));
        self
    }

    /// Send every request for `issuer` to the endpoints of `provider` (a fake
    /// authorization server) while the scopes and registration still come from the
    /// client table, as they do in production.
    #[must_use]
    pub fn with_issuer_endpoints(mut self, issuer: OAuthIssuer, provider: Provider) -> Self {
        self.endpoints.insert(issuer, provider);
        self
    }

    /// Use `table` instead of the committed client registrations
    /// ([`ClientTable::defaults`]); environment overrides still apply.
    #[must_use]
    pub fn with_client_table(mut self, table: ClientTable) -> Self {
        self.clients = Arc::new(table);
        self
    }

    /// Start from `config` (the provider crate's serde configuration: a fake
    /// server's API origin, a chosen folder) instead of the provider's default when
    /// resolving a new account's storage.
    #[must_use]
    pub fn with_storage_config(mut self, cloud: OAuthCloud, config: serde_json::Value) -> Self {
        self.storage.insert(cloud.type_id(), config);
        self
    }

    /// The non-secret mount configuration of a newly signed-in account, asked of the
    /// provider once with its fresh access token.
    pub fn resolve_storage(
        &self,
        cloud: OAuthCloud,
        tokens: &TokenSet,
    ) -> Result<serde_json::Value, CredentialError> {
        if cloud == OAuthCloud::GoogleDrive
            && let Some(granted) = &tokens.scope
        {
            let (provider, _) = self.oauth_config(cloud)?;
            if provider
                .scopes
                .iter()
                .any(|required| !granted.split_whitespace().any(|scope| scope == required))
            {
                return Err(CredentialError::SignIn(
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

    /// The account a finished cloud sign-in makes, named `label`, and the secret it keeps: its
    /// storage resolved once (the drive, its root), as every reader of the account opens it. The
    /// installer and `tilo migrate connect` both make their accounts here, and publish them with
    /// the secret sealed first.
    ///
    /// # Errors
    /// The grant lacks the permission files need, or the provider could not be asked.
    pub fn cloud_account(
        &self,
        cloud: OAuthCloud,
        label: &str,
        tokens: &TokenSet,
    ) -> Result<(Account, Secret), CredentialError> {
        let mount = self.resolve_storage(cloud, tokens)?;
        let mut account = Account::new(
            AccountId::new(uuid::Uuid::new_v4().to_string()),
            cloud.type_id(),
        );
        account.display_name = label.chars().take(256).collect();
        account.config = serde_json::json!({
            "provider": cloud.type_id(),
            accounts::cloud_types::MOUNT_CONFIG_KEY: mount,
        });
        Ok((account, oauth_secret(tokens)))
    }

    /// The `iCloud` Drive account a finished sign-in of `apple_id` makes, and the secret it keeps.
    ///
    /// # Errors
    /// `apple_id` is empty or longer than an Apple ID is.
    pub fn icloud_account(
        &self,
        apple_id: &str,
        session: &ICloudSessionState,
    ) -> Result<(Account, Secret), CredentialError> {
        if apple_id.trim().is_empty() || apple_id.len() > 256 {
            return Err(CredentialError::Account("an Apple ID is needed".into()));
        }
        let mut account = Account::new(
            AccountId::new(uuid::Uuid::new_v4().to_string()),
            ICLOUD_DRIVE_TYPE,
        );
        apple_id.clone_into(&mut account.display_name);
        let mut config = serde_json::Map::new();
        config.insert("identity".into(), apple_id.into());
        let mount = self.icloud.storage_config();
        if !mount.is_null() {
            config.insert(accounts::cloud_types::MOUNT_CONFIG_KEY.into(), mount);
        }
        account.config = serde_json::Value::Object(config);
        Ok((account, icloud_secret(apple_id, session)))
    }

    /// Use Apple's `iCloud` web sign-in ([`crate::icloud::WebICloud`]) for `iCloud`
    /// Drive; Settings and the credential helper run with it.
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

    /// The person's vault under this store's root, refusing one that cannot be read rather than
    /// reading it as empty.
    fn credentials(&self) -> Result<SecretVault, CredentialError> {
        let vault = SecretVault::open_person(self.config_root.as_deref());
        match vault.recovery_error() {
            Some(error) => Err(CredentialError::Storage(format!(
                "vault recovery required; restore access and reopen: {error}"
            ))),
            None => Ok(vault),
        }
    }

    /// The descriptor and registration for a storage provider, or the "not
    /// configured" state.
    pub fn oauth_config(
        &self,
        cloud: OAuthCloud,
    ) -> Result<(Provider, Registration), CredentialError> {
        self.use_config(cloud.oauth_use())
    }

    /// The descriptor and registration for a use, or the "not configured" state.
    pub fn use_config(
        &self,
        oauth_use: OAuthUse,
    ) -> Result<(Provider, Registration), CredentialError> {
        if let Some(configured) = self.overrides.get(&oauth_use) {
            return Ok(configured.clone());
        }
        let (descriptor, registration) =
            self.clients
                .config(oauth_use)
                .ok_or_else(|| CredentialError::NotConfigured {
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

    pub fn is_configured(&self, oauth_use: OAuthUse) -> bool {
        self.use_config(oauth_use).is_ok()
    }

    pub fn begin_sign_in(
        &self,
        cloud: OAuthCloud,
        login_hint: Option<&str>,
    ) -> Result<Authorization, CredentialError> {
        self.begin_sign_in_for(cloud.oauth_use(), login_hint)
    }

    pub fn begin_sign_in_for(
        &self,
        oauth_use: OAuthUse,
        login_hint: Option<&str>,
    ) -> Result<Authorization, CredentialError> {
        let (provider, registration) = self.use_config(oauth_use)?;
        Authorization::begin(&provider, &registration, login_hint).map_err(CredentialError::from)
    }

    pub fn begin_google_sign_in(&self, mail: bool) -> Result<Authorization, CredentialError> {
        let capabilities = if mail {
            &[crate::Capability::Files, crate::Capability::Mail][..]
        } else {
            &[crate::Capability::Files][..]
        };
        self.begin_sign_in_for_capabilities(OAuthIssuer::Google, capabilities, None)
    }

    pub fn begin_google_services_sign_in(
        &self,
        files: bool,
        mail: bool,
    ) -> Result<Authorization, CredentialError> {
        let mut capabilities = Vec::new();
        if files {
            capabilities.push(crate::Capability::Files);
        }
        if mail {
            capabilities.push(crate::Capability::Mail);
        }
        self.begin_sign_in_for_capabilities(OAuthIssuer::Google, &capabilities, None)
    }

    pub fn begin_sign_in_for_capabilities(
        &self,
        issuer: OAuthIssuer,
        capabilities: &[crate::Capability],
        login_hint: Option<&str>,
    ) -> Result<Authorization, CredentialError> {
        let Some((&first, rest)) = capabilities.split_first() else {
            return Err(CredentialError::SignIn(
                "Select at least one capability".into(),
            ));
        };
        if issuer == OAuthIssuer::Microsoft
            && capabilities.contains(&crate::Capability::Mail)
            && capabilities
                .iter()
                .any(|use_| *use_ != crate::Capability::Mail)
        {
            return Err(CredentialError::SignIn(
                "Microsoft mail and Graph require separate sign-ins".into(),
            ));
        }
        let (mut provider, registration) = self.use_config(OAuthUse::new(issuer, first))?;
        for &capability in rest {
            let (mut additional, client) = self.use_config(OAuthUse::new(issuer, capability))?;
            let scopes = std::mem::take(&mut additional.scopes);
            let mut canonical = provider.clone();
            canonical.scopes.clear();
            if additional != canonical || client != registration {
                return Err(CredentialError::SignIn(
                    "Selected capabilities have incompatible OAuth configurations".into(),
                ));
            }
            for scope in scopes {
                if !provider.scopes.contains(&scope) {
                    provider.scopes.push(scope);
                }
            }
        }
        Authorization::begin(&provider, &registration, login_hint).map_err(CredentialError::from)
    }

    #[must_use]
    pub fn with_microsoft_profile_endpoint(mut self, endpoint: String) -> Self {
        self.microsoft_profile_endpoint = endpoint;
        self
    }

    pub fn microsoft_mail_identity(&self, tokens: &TokenSet) -> Result<String, CredentialError> {
        let http = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CredentialError::Offline)?;
        let response = http
            .get(&self.microsoft_profile_endpoint)
            .bearer_auth(&tokens.access_token)
            .send()
            .map_err(|_| CredentialError::Offline)?;
        if !response.status().is_success() {
            return Err(CredentialError::SignIn(format!(
                "Microsoft mailbox identity could not be read (HTTP {})",
                response.status().as_u16()
            )));
        }
        let profile: serde_json::Value = response
            .json()
            .map_err(|_| CredentialError::SignIn("Microsoft returned an invalid profile".into()))?;
        let email = profile
            .get("mail")
            .and_then(serde_json::Value::as_str)
            .filter(|email| valid_mailbox(email))
            .ok_or_else(|| {
                CredentialError::SignIn("Microsoft returned no mailbox address".into())
            })?;
        Ok(email.to_string())
    }

    #[must_use]
    pub fn with_gmail_profile_endpoint(mut self, endpoint: String) -> Self {
        self.gmail_profile_endpoint = endpoint;
        self
    }

    pub fn gmail_identity(&self, tokens: &TokenSet) -> Result<String, CredentialError> {
        if tokens.scope.as_deref().is_some_and(|scopes| {
            !scopes
                .split_whitespace()
                .any(|scope| scope == "https://mail.google.com/")
        }) {
            return Err(CredentialError::SignIn(
                "Gmail permission was not granted".into(),
            ));
        }
        let http = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CredentialError::Offline)?;
        let response = http
            .get(&self.gmail_profile_endpoint)
            .bearer_auth(&tokens.access_token)
            .send()
            .map_err(|_| CredentialError::Offline)?;
        if !response.status().is_success() {
            return Err(CredentialError::SignIn(format!(
                "Gmail could not be opened (HTTP {}). Check Gmail availability and mail permission.",
                response.status().as_u16()
            )));
        }
        let profile: serde_json::Value = response
            .json()
            .map_err(|_| CredentialError::SignIn("Gmail returned an invalid profile".into()))?;
        let email = profile
            .get("emailAddress")
            .and_then(serde_json::Value::as_str)
            .filter(|email| valid_mailbox(email))
            .ok_or_else(|| CredentialError::SignIn("Gmail returned no mailbox address".into()))?;
        Ok(email.to_string())
    }

    pub fn begin_sign_in_for_scopes(
        &self,
        oauth_use: OAuthUse,
        scopes: &[&str],
        login_hint: Option<&str>,
    ) -> Result<Authorization, CredentialError> {
        let (mut provider, registration) = self.use_config(oauth_use)?;
        if scopes.is_empty()
            || scopes
                .iter()
                .any(|scope| !provider.scopes.iter().any(|allowed| allowed == scope))
        {
            return Err(CredentialError::SignIn(
                "Requested scopes are not configured for this use".into(),
            ));
        }
        provider.scopes = scopes.iter().map(|scope| (*scope).to_string()).collect();
        Authorization::begin(&provider, &registration, login_hint).map_err(CredentialError::from)
    }

    pub fn oauth_scopes(&self, id: &AccountId) -> Result<Option<String>, CredentialError> {
        let vault = self.credentials()?;
        let secret = vault.get(id).ok_or(CredentialError::NeedsAuthentication)?;
        let Some((Grant::Oauth2 { tokens, .. }, _)) = decode_grant(&secret) else {
            return Err(CredentialError::NeedsAuthentication);
        };
        Ok(tokens.scope)
    }

    pub fn oauth_grant_state(&self, id: &AccountId) -> Result<GrantState, CredentialError> {
        let vault = self.credentials()?;
        let Some(secret) = vault.get(id) else {
            return Ok(GrantState::Missing);
        };
        Ok(match decode_grant(&secret) {
            Some((Grant::Oauth2 { tokens, .. }, _)) if !tokens.refresh_token.is_empty() => {
                GrantState::Usable
            }
            _ => GrantState::SignInAgain,
        })
    }

    pub fn seal_oauth(&self, id: &AccountId, tokens: &TokenSet) -> Result<(), CredentialError> {
        let mut credentials = self.credentials()?;
        credentials.put(id, &oauth_secret(tokens));
        credentials.try_save().map_err(CredentialError::Storage)
    }

    pub fn forget_oauth(&self, id: &AccountId) -> Result<(), CredentialError> {
        let mut vault = self.credentials()?;
        vault.remove(id);
        vault.try_save().map_err(CredentialError::Storage)
    }

    pub fn oauth_access_token(
        &self,
        id: &AccountId,
        oauth_use: OAuthUse,
        force: bool,
    ) -> Result<Zeroizing<String>, CredentialError> {
        self.oauth_access_token_with_expiry(id, oauth_use, force)
            .map(|(token, _)| token)
    }

    pub fn oauth_access_token_with_expiry(
        &self,
        id: &AccountId,
        oauth_use: OAuthUse,
        force: bool,
    ) -> Result<(Zeroizing<String>, Option<u64>), CredentialError> {
        let mut vault = self.credentials()?;
        let tokens = self.fresh_oauth_tokens(&mut vault, id, oauth_use, force)?;
        let expires = tokens.expires_at();
        Ok((Zeroizing::new(tokens.access_token), expires))
    }

    fn fresh_oauth_tokens(
        &self,
        credentials: &mut SecretVault,
        id: &AccountId,
        oauth_use: OAuthUse,
        force: bool,
    ) -> Result<TokenSet, CredentialError> {
        let secret = credentials
            .get(id)
            .ok_or(CredentialError::NeedsAuthentication)?;
        let renewed = self.renew_grant(&secret, oauth_use, force)?;
        if let Some(keep) = &renewed.keep {
            credentials.put(id, keep);
            credentials.try_save().map_err(CredentialError::Storage)?;
        }
        renewed.tokens.ok_or(CredentialError::NeedsAuthentication)
    }

    /// The grant `secret` keeps for `oauth_use`, renewed first where it is about to expire, or
    /// with `force`: the one way a grant is renewed, whatever keeps the secret. The answer says
    /// what to keep in the secret's place where that changed; it holds no tokens where the
    /// provider will not renew the grant any more, and its person signs in again.
    ///
    /// # Errors
    /// `secret` holds no grant, the grant's registration does not fit the use, or the provider
    /// was not reached.
    pub fn renew_grant(
        &self,
        secret: &Secret,
        oauth_use: OAuthUse,
        force: bool,
    ) -> Result<RenewedGrant, CredentialError> {
        let Some((
            Grant::Oauth2 {
                mut tokens,
                copied,
                copy_origin,
            },
            legacy,
        )) = decode_grant(secret)
        else {
            return Err(CredentialError::NeedsAuthentication);
        };
        let mut keep = legacy.then(|| oauth_secret(&tokens));
        if tokens.refresh_token.is_empty() {
            return Ok(RenewedGrant { tokens: None, keep });
        }
        if force || tokens.needs_refresh() {
            let (provider, client) = self.use_config(oauth_use)?;
            let mut renewed =
                match OAuthClient::new()?.refresh(&provider, &client, &tokens.refresh_token) {
                    Ok(renewed) => renewed.stamp_now(),
                    Err(OAuthError::InvalidGrant) => {
                        return Ok(RenewedGrant {
                            tokens: None,
                            keep: Some(registered_secret(
                                &TokenSet::default(),
                                copied,
                                copy_origin,
                            )),
                        });
                    }
                    Err(error) => return Err(error.into()),
                };
            if renewed.scope.is_none() {
                renewed.scope = tokens.scope;
            }
            tokens = renewed;
            keep = Some(registered_secret(&tokens, copied, copy_origin));
        }
        Ok(RenewedGrant {
            tokens: Some(tokens),
            keep,
        })
    }

    pub fn finish_sign_in(authorization: Authorization) -> Result<TokenSet, CredentialError> {
        let client = OAuthClient::new()?;
        let tokens = authorization.complete(&client, SIGN_IN_TIMEOUT)?;
        if tokens.refresh_token.is_empty() {
            return Err(CredentialError::SignIn(
                "the provider did not grant offline access; try again".into(),
            ));
        }
        Ok(tokens)
    }

    pub fn begin_device_sign_in(
        &self,
        oauth_use: OAuthUse,
    ) -> Result<DeviceAuthorization, CredentialError> {
        let (provider, registration) = self.use_config(oauth_use)?;
        Ok(DeviceAuthorization::begin(
            &OAuthClient::new()?,
            &provider,
            &registration,
        )?)
    }

    pub fn finish_device_sign_in(
        authorization: DeviceAuthorization,
    ) -> Result<TokenSet, CredentialError> {
        let tokens = authorization.complete(&OAuthClient::new()?)?;
        if tokens.refresh_token.is_empty() {
            return Err(CredentialError::SignIn(
                "the provider did not grant offline access; try again".into(),
            ));
        }
        Ok(tokens)
    }

    pub fn finish_sign_in_with_code(
        authorization: Authorization,
        code: &str,
    ) -> Result<TokenSet, CredentialError> {
        let client = OAuthClient::new()?;
        let tokens = authorization.complete_with_code(&client, code)?;
        if tokens.refresh_token.is_empty() {
            return Err(CredentialError::SignIn(
                "the provider did not grant offline access; try again".into(),
            ));
        }
        Ok(tokens)
    }

    /// What `account`'s drive signs in with, from the sign-in `secret` the workspace's vault
    /// keeps for it, and the secret to keep in its place where it was renewed: an `OAuth`
    /// account's current access token, renewed when it is due (or when `force`, after the drive
    /// refused it), or an iCloud account's session headers. No vault is opened here: whoever
    /// keeps the account keeps the renewed secret too. A grant the provider no longer takes
    /// signs no drive in ([`DriveCredential::credential`] is `None`, and the sign-in it was is
    /// kept where it is): its person signs in again.
    ///
    /// # Errors
    /// The account is disabled, signs in with no grant, or its grant was not renewed.
    pub fn drive_credential(
        &self,
        account: &Account,
        secret: &Secret,
        force: bool,
    ) -> Result<DriveCredential, CredentialError> {
        if !account.enabled {
            return Err(CredentialError::Account("account is disabled".into()));
        }
        if matches!(account.type_id.as_str(), WEBDAV_TYPE | NEXTCLOUD_TYPE) {
            // Its password is the workspace's vault entry's, which the accounts provider
            // releases where the account is: no grant signs it in.
            return Err(CredentialError::NeedsAuthentication);
        }
        let (credential, renewed) = if let Some(cloud) =
            OAuthCloud::from_type_id(account.type_id.as_str())
        {
            let renewing = self.renew_grant(secret, cloud.oauth_use(), force)?;
            let Some(tokens) = renewing.tokens else {
                // The provider will not renew it any more: what is left is kept so its
                // person is asked to sign in again.
                return Ok(DriveCredential {
                    credential: None,
                    renewed: renewing.keep,
                });
            };
            (
                Some(cloud_source::Credential::Bearer {
                    token: Zeroizing::new(tokens.access_token.clone()),
                    expires: expires_at(tokens.expires_at()),
                }),
                renewing.keep,
            )
        } else if account.type_id.as_str() == ICLOUD_DRIVE_TYPE {
            let Some(Grant::Icloud {
                apple_id,
                session,
                copied,
                copy_origin,
            }) = grant_of(secret)
            else {
                return Err(CredentialError::NeedsAuthentication);
            };
            let (renewed, headers) = match self
                .icloud
                .resume(&ICloudSessionState(Zeroizing::new(session)))
            {
                Ok(resumed) => resumed,
                Err(
                    ICloudAuthError::NeedsReauthentication | ICloudAuthError::InvalidCredentials,
                ) => {
                    // The session is no longer taken: it is left where it is, so its person is
                    // asked to sign in again, as a grant the provider will not renew.
                    return Ok(DriveCredential {
                        credential: None,
                        renewed: None,
                    });
                }
                Err(ICloudAuthError::Offline) => return Err(CredentialError::Offline),
                Err(other) => return Err(CredentialError::SignIn(other.to_string())),
            };
            (
                Some(cloud_source::Credential::Session {
                    headers: headers.headers,
                    expires: expires_at(headers.expires_unix),
                }),
                Some(icloud_secret_with_provenance(
                    &apple_id,
                    &renewed,
                    copied,
                    copy_origin,
                )),
            )
        } else {
            return Err(CredentialError::Account(
                "this account type is not a file provider".into(),
            ));
        };
        Ok(DriveCredential {
            credential,
            renewed,
        })
    }

    pub fn revoke(&self, account: &Account, secret: &Secret) {
        if is_copied_secret(secret) || (has_grant_tag(secret) && grant_of(secret).is_none()) {
            return;
        }
        let type_id = account.type_id.as_str();
        if let Some(cloud) = OAuthCloud::from_type_id(type_id) {
            let Some(Grant::Oauth2 { tokens, .. }) = grant_of(secret) else {
                return;
            };
            let Ok((provider, registration)) = self.use_config(cloud.oauth_use()) else {
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

/// A moment `unix` seconds after the epoch.
fn expires_at(unix: Option<u64>) -> Option<std::time::SystemTime> {
    unix.and_then(|secs| std::time::UNIX_EPOCH.checked_add(Duration::from_secs(secs)))
}

/// What a cloud mount is handed for `credential` over the workspace's accounts action
/// (`tilo_actions::remote::person_accounts::CLOUD_CREDENTIAL`): the same sign-in, in the shape every mount reads.
#[must_use]
pub fn for_mount(credential: cloud_source::Credential) -> CloudCredential {
    let expires_at_ms = match &credential {
        cloud_source::Credential::Bearer { expires, .. }
        | cloud_source::Credential::Session { expires, .. } => expires.and_then(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|since| u64::try_from(since.as_millis()).ok())
        }),
        cloud_source::Credential::Basic { .. } => None,
    };
    match credential {
        cloud_source::Credential::Basic { username, password } => CloudCredential {
            kind: CloudCredentialKind::Basic,
            username,
            secret: password.to_string(),
            headers: Vec::new(),
            expires_at_ms,
        },
        cloud_source::Credential::Bearer { token, .. } => CloudCredential {
            kind: CloudCredentialKind::Bearer,
            username: String::new(),
            secret: token.to_string(),
            headers: Vec::new(),
            expires_at_ms,
        },
        cloud_source::Credential::Session { headers, .. } => CloudCredential {
            kind: CloudCredentialKind::Session,
            username: String::new(),
            secret: String::new(),
            headers: headers
                .into_iter()
                .map(|(name, value)| CloudHeader {
                    name,
                    value: value.to_string(),
                })
                .collect(),
            expires_at_ms,
        },
    }
}

/// A drive's credential, and the sign-in to keep in its place where it was renewed
/// ([`CredentialStore::drive_credential`]). Prints neither.
pub struct DriveCredential {
    /// What the drive signs in with now; none where the provider will not renew the grant any
    /// more, and its person signs in again.
    pub credential: Option<cloud_source::Credential>,
    /// What to keep where the secret read was, when the grant changed: a renewed one, or
    /// the emptied one a provider will not renew any more.
    pub renewed: Option<Secret>,
}

impl std::fmt::Debug for DriveCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DriveCredential")
            .field("credential", &self.credential)
            .field("renewed", &self.renewed.is_some())
            .finish()
    }
}
