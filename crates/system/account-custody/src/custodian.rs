//! The custodian: the one place that turns a sealed long-lived grant into a
//! short-lived credential, for the storage daemon (through the handoff
//! store) and for Mail (an access token for `XOAUTH2`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use accounts::{Account, AccountCredentials, AccountId, HandoffCredential, HandoffStore, Secret};
use serde::{Deserialize, Serialize};
use tilo_oauth::client::{Authorization, OAuthClient};
use tilo_oauth::{OAuthError, Provider, Registration, TokenSet};
use zeroize::{Zeroize, Zeroizing};

use crate::clients::ClientTable;
use crate::icloud::{ICloudAuth, ICloudAuthError, ICloudSessionState, UnavailableICloud};
use crate::providers::{
    ICLOUD_DRIVE_TYPE, NEXTCLOUD_TYPE, OAuthCloud, OAuthIssuer, OAuthUse, WEBDAV_TYPE,
    valid_mailbox,
};

/// The protected grant wire protocol implemented by this loaded custodian.
///
/// Native runtime certification invokes this non-inlined symbol through the
/// actual SDK dylib, separately from the installed credential helper. It is
/// not a package version or a claim made by a prepared-account importer.
#[inline(never)]
pub fn protected_grant_protocol() -> u32 {
    1
}

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
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Grant {
    #[serde(alias = "protected_oauth2")]
    Oauth2 {
        tokens: TokenSet,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        registration: Option<ImportedRegistration>,
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

// Protected kinds deliberately differ from the legacy decoder's known variants.
// Their credentials stay nested: an older decoder cannot fall back to TokenSet
// or use/revoke an outer plaintext password after ignoring copy provenance.
impl Serialize for Grant {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        match self {
            Self::Oauth2 {
                tokens,
                registration,
                copied,
                copy_origin,
            } => {
                let protected = *copied || registration.is_some() || copy_origin.is_some();
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
                    map.serialize_entry("registration", registration)?;
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

/// Original installed-app client registration, sealed with an imported cloud grant.
/// This never contains provider endpoints and cannot override another issuer or use.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedRegistration {
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
    source_binding: String,
}

impl Drop for ImportedRegistration {
    fn drop(&mut self) {
        if let Some(secret) = &mut self.client_secret {
            secret.zeroize();
        }
    }
}

impl ImportedRegistration {
    /// Bind an explicitly imported Files grant to its original client and source.
    ///
    /// # Errors
    /// Refuses empty, unbounded or control-containing client/source values.
    pub fn new(
        cloud: OAuthCloud,
        client: Registration,
        source_binding: String,
    ) -> Result<Self, CustodyError> {
        let value = Self {
            issuer: cloud.issuer().id().into(),
            client_id: client.client_id,
            client_secret: client.client_secret,
            source_binding,
        };
        value.validate(cloud.oauth_use())?;
        Ok(value)
    }

    fn validate(&self, oauth_use: OAuthUse) -> Result<(), CustodyError> {
        let valid = |s: &str| !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control);
        if oauth_use.capability != crate::Capability::Files
            || self.issuer != oauth_use.issuer.id()
            || !matches!(
                oauth_use.issuer,
                OAuthIssuer::Google | OAuthIssuer::Microsoft | OAuthIssuer::Dropbox
            )
            || !valid(&self.client_id)
            || !valid(&self.source_binding)
            || self.client_secret.as_deref().is_some_and(|s| !valid(s))
        {
            return Err(CustodyError::NeedsAuthentication);
        }
        Ok(())
    }
}

fn registered_secret(
    tokens: &TokenSet,
    registration: Option<ImportedRegistration>,
    copied: bool,
    copy_origin: Option<CopyOrigin>,
) -> Secret {
    Secret {
        password: String::new(),
        token: serde_json::to_string(&Grant::Oauth2 {
            tokens: tokens.clone(),
            registration,
            copied,
            copy_origin,
        })
        .ok(),
    }
}

/// Seal the original client registration with its cloud grant.
pub fn imported_oauth_secret(tokens: &TokenSet, registration: &ImportedRegistration) -> Secret {
    registered_secret(tokens, Some(registration.clone()), true, None)
}

/// Test an existing sealed imported grant's exact original source/client binding.
pub fn imported_oauth_matches(secret: &Secret, registration: &ImportedRegistration) -> bool {
    matches!(grant_of(secret), Some(Grant::Oauth2 { tokens, registration: Some(stored), .. })
        if !tokens.refresh_token.is_empty() && stored.issuer == registration.issuer && stored.client_id == registration.client_id
        && stored.client_secret == registration.client_secret && stored.source_binding == registration.source_binding)
}

/// Seal an `OAuth` sign-in result into a vault secret.
pub fn oauth_secret(tokens: &TokenSet) -> Secret {
    Secret {
        password: String::new(),
        token: serde_json::to_string(&Grant::Oauth2 {
            tokens: tokens.clone(),
            registration: None,
            copied: false,
            copy_origin: None,
        })
        .ok(),
    }
}

/// Seal an `iCloud` session into a vault secret. The Apple ID password is
/// not part of it.
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

/// Copy canonical custody without transferring authority to revoke the original sign-in.
/// The grant, original registration and password remain usable; this provenance is
/// sealed and retained through refresh and portable resealing.
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
        Some(Grant::Oauth2 {
            tokens,
            registration,
            ..
        }) => Some(Grant::Oauth2 {
            tokens,
            registration,
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

/// Match an explicitly copied source after its target access/refresh token changed.
/// Both original credential and owner/account metadata fingerprints must match.
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

/// Read a password through canonical custody, including protected copied DAV grants.
/// Other account types retain their existing plain password representation.
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

/// Whether preserving this grant's client/copy authority requires protocol 1.
pub fn requires_protected_consumer(secret: &Secret) -> bool {
    match grant_of(secret) {
        Some(Grant::Oauth2 {
            registration,
            copied,
            copy_origin,
            ..
        }) => copied || registration.is_some() || copy_origin.is_some(),
        Some(Grant::Icloud {
            copied,
            copy_origin,
            ..
        }) => copied || copy_origin.is_some(),
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

/// A sealed grant, and whether it is in the format Mail sealed before its
/// grants moved into custody: the bare [`TokenSet`] JSON, without the
/// `kind` tag. Such a grant reads as [`Grant::Oauth2`] and is resealed in
/// the current format on first use.
fn decode_grant(secret: &Secret) -> Option<(Grant, bool)> {
    let raw = secret.token.as_deref()?;
    if let Ok(grant) = serde_json::from_str(raw) {
        return Some((grant, false));
    }
    // Tagged objects are never legacy bare tokens, even if they contain an
    // access_token field. Refuse unknown future protected custody formats.
    if has_grant_tag(secret) {
        return None;
    }
    serde_json::from_str::<TokenSet>(raw).ok().map(|tokens| {
        (
            Grant::Oauth2 {
                tokens,
                registration: None,
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
    microsoft_profile_endpoint: String,
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
            microsoft_profile_endpoint: "https://graph.microsoft.com/v1.0/me?$select=mail".into(),
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

    fn credentials(&self) -> Result<AccountCredentials, CustodyError> {
        match &self.config_root {
            Some(root) => AccountCredentials::open_in(root),
            None => AccountCredentials::open(),
        }
        .map_err(CustodyError::Storage)
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

    fn grant_config(
        &self,
        oauth_use: OAuthUse,
        imported: Option<&ImportedRegistration>,
    ) -> Result<(Provider, Registration), CustodyError> {
        let Some(imported) = imported else {
            return self.use_config(oauth_use);
        };
        imported.validate(oauth_use)?;
        // Descriptors remain canonical. The sealed grant supplies only its own client.
        let provider = self
            .overrides
            .get(&oauth_use)
            .map(|(provider, _)| provider.clone())
            .or_else(|| self.endpoints.get(&oauth_use.issuer).cloned())
            .unwrap_or_else(|| oauth_use.issuer.endpoints());
        let mut client = Registration::public(imported.client_id.clone());
        client.client_secret.clone_from(&imported.client_secret);
        Ok((provider, client))
    }

    /// Resolve the same selected remote root using the canonical provider consumer.
    ///
    /// # Errors
    /// Refuses invalid provider configuration, unavailable storage or inaccessible grants.
    pub fn resolve_imported_storage(
        &self,
        cloud: OAuthCloud,
        tokens: &TokenSet,
        root: Option<&str>,
        drive: Option<&str>,
    ) -> Result<serde_json::Value, CustodyError> {
        let mut config =
            self.storage
                .get(cloud.type_id())
                .cloned()
                .unwrap_or_else(|| match cloud {
                    OAuthCloud::GoogleDrive => {
                        serde_json::to_value(cloud_source_gdrive::DriveConfig::my_drive())
                            .unwrap_or_default()
                    }
                    OAuthCloud::OneDrive => {
                        serde_json::to_value(cloud_source_onedrive::OneDriveConfig::me())
                            .unwrap_or_default()
                    }
                    OAuthCloud::Dropbox => {
                        serde_json::to_value(cloud_source_dropbox::DropboxConfig::new(""))
                            .unwrap_or_default()
                    }
                });
        if root.is_some() || drive.is_some() {
            // A null configuration accepts selected fields just like an empty object.
            if config.is_null() {
                config = serde_json::json!({});
            }
            let fields = config
                .as_object_mut()
                .ok_or(CustodyError::NeedsAuthentication)?;
            if let Some(root) = root {
                fields.insert(
                    "root".into(),
                    match cloud {
                        OAuthCloud::OneDrive => serde_json::json!({"Item":root}),
                        OAuthCloud::GoogleDrive | OAuthCloud::Dropbox => serde_json::json!(root),
                    },
                );
            }
            if let Some(drive) = drive {
                let (key, value) = match cloud {
                    OAuthCloud::GoogleDrive => ("shared_drive", serde_json::json!(drive)),
                    OAuthCloud::OneDrive => ("drive", serde_json::json!({"Id":drive})),
                    OAuthCloud::Dropbox => return Err(CustodyError::NeedsAuthentication),
                };
                fields.insert(key.into(), value);
            }
        }
        crate::storage::resolve(cloud, Some(&config), &tokens.access_token)
    }

    /// Validate an explicitly selected imported refresh grant under its original client.
    ///
    /// # Errors
    /// Invalid, revoked, mismatched or unreachable grants are refused.
    pub fn refresh_imported_cloud(
        &self,
        cloud: OAuthCloud,
        tokens: &TokenSet,
        registration: &ImportedRegistration,
    ) -> Result<TokenSet, CustodyError> {
        if tokens.refresh_token.is_empty() {
            return Err(CustodyError::NeedsAuthentication);
        }
        let (provider, client) = self.grant_config(cloud.oauth_use(), Some(registration))?;
        Ok(OAuthClient::new()?
            .refresh(&provider, &client, &tokens.refresh_token)?
            .stamp_now())
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
        let capabilities = if mail {
            &[crate::Capability::Files, crate::Capability::Mail][..]
        } else {
            &[crate::Capability::Files][..]
        };
        self.begin_sign_in_for_capabilities(OAuthIssuer::Google, capabilities, None)
    }

    /// Start Google sign-in for only the selected files and mail services.
    ///
    /// # Errors
    /// No selected service, incompatible registrations, or a loopback failure.
    pub fn begin_google_services_sign_in(
        &self,
        files: bool,
        mail: bool,
    ) -> Result<Authorization, CustodyError> {
        let mut capabilities = Vec::new();
        if files {
            capabilities.push(crate::Capability::Files);
        }
        if mail {
            capabilities.push(crate::Capability::Mail);
        }
        self.begin_sign_in_for_capabilities(OAuthIssuer::Google, &capabilities, None)
    }

    /// Start sign-in for exactly the selected capabilities, without implicitly
    /// adding files. Google can combine files, mail and `CalDAV` in one consent.
    /// Every selected use must be configured under the same registration and
    /// endpoints. Microsoft Exchange mail must sign in separately from Graph
    /// identity/files: those resources require different audience tokens.
    ///
    /// # Errors
    /// Empty selection, incompatible configurations, unsupported capabilities,
    /// mixed Microsoft resources, or a loopback failure.
    pub fn begin_sign_in_for_capabilities(
        &self,
        issuer: OAuthIssuer,
        capabilities: &[crate::Capability],
        login_hint: Option<&str>,
    ) -> Result<Authorization, CustodyError> {
        let Some((&first, rest)) = capabilities.split_first() else {
            return Err(CustodyError::SignIn(
                "Select at least one capability".into(),
            ));
        };
        if issuer == OAuthIssuer::Microsoft
            && capabilities.contains(&crate::Capability::Mail)
            && capabilities
                .iter()
                .any(|use_| *use_ != crate::Capability::Mail)
        {
            return Err(CustodyError::SignIn(
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
                return Err(CustodyError::SignIn(
                    "Selected capabilities have incompatible OAuth configurations".into(),
                ));
            }
            for scope in scopes {
                if !provider.scopes.contains(&scope) {
                    provider.scopes.push(scope);
                }
            }
        }
        Authorization::begin(&provider, &registration, login_hint).map_err(CustodyError::from)
    }

    /// Replace the Microsoft Graph identity endpoint for a local fixture.
    #[must_use]
    pub fn with_microsoft_profile_endpoint(mut self, endpoint: String) -> Self {
        self.microsoft_profile_endpoint = endpoint;
        self
    }

    /// Obtain Microsoft's confirmed mailbox address using a **Graph identity**
    /// token, obtained with `OAuthUse::identity(Microsoft)` (or files with
    /// `User.Read`), not an Exchange mail token. Sign in for mail separately
    /// with this address as the login hint, then validate that mail connection
    /// before publishing it. A UPN is not necessarily a mailbox; do not guess.
    ///
    /// # Errors
    /// Graph permission denied, unavailable service, or no mailbox address.
    pub fn microsoft_mail_identity(&self, tokens: &TokenSet) -> Result<String, CustodyError> {
        let http = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CustodyError::Offline)?;
        let response = http
            .get(&self.microsoft_profile_endpoint)
            .bearer_auth(&tokens.access_token)
            .send()
            .map_err(|_| CustodyError::Offline)?;
        if !response.status().is_success() {
            return Err(CustodyError::SignIn(format!(
                "Microsoft mailbox identity could not be read (HTTP {})",
                response.status().as_u16()
            )));
        }
        let profile: serde_json::Value = response
            .json()
            .map_err(|_| CustodyError::SignIn("Microsoft returned an invalid profile".into()))?;
        let email = profile
            .get("mail")
            .and_then(serde_json::Value::as_str)
            .filter(|email| valid_mailbox(email))
            .ok_or_else(|| CustodyError::SignIn("Microsoft returned no mailbox address".into()))?;
        Ok(email.to_string())
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
            .filter(|email| valid_mailbox(email))
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
        let vault = self.credentials()?;
        let secret = vault.get(id).ok_or(CustodyError::NeedsAuthentication)?;
        let Some((Grant::Oauth2 { tokens, .. }, _)) = decode_grant(&secret) else {
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

    /// Seal a sign-in result as `id`'s grant, replacing any earlier one.
    ///
    /// # Errors
    /// [`CustodyError::Storage`] when the vault cannot be written.
    pub fn seal_oauth(&self, id: &AccountId, tokens: &TokenSet) -> Result<(), CustodyError> {
        let mut credentials = self.credentials()?;
        credentials.put(id, &oauth_secret(tokens));
        credentials.try_save().map_err(CustodyError::Storage)
    }

    /// Forget a local OAuth grant. Provider-side revocation remains separate.
    ///
    /// # Errors
    /// A vault recovery or persistence failure.
    pub fn forget_oauth(&self, id: &AccountId) -> Result<(), CustodyError> {
        let mut vault = self.credentials()?;
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
        let mut vault = self.credentials()?;
        let tokens = self.fresh_oauth_tokens(&mut vault, id, oauth_use, force)?;
        let expires = tokens.expires_at();
        Ok((Zeroizing::new(tokens.access_token), expires))
    }

    /// `id`'s sealed `OAuth` tokens, refreshed when due (or `force`), with
    /// any change sealed back into `credentials`.
    fn fresh_oauth_tokens(
        &self,
        credentials: &mut AccountCredentials,
        id: &AccountId,
        oauth_use: OAuthUse,
        force: bool,
    ) -> Result<TokenSet, CustodyError> {
        let secret = credentials
            .get(id)
            .ok_or(CustodyError::NeedsAuthentication)?;
        let Some((
            Grant::Oauth2 {
                mut tokens,
                registration,
                copied,
                copy_origin,
            },
            legacy,
        )) = decode_grant(&secret)
        else {
            return Err(CustodyError::NeedsAuthentication);
        };
        if let Some(imported) = &registration {
            imported.validate(oauth_use)?;
        }
        if legacy {
            credentials.put(id, &oauth_secret(&tokens));
            credentials.try_save().map_err(CustodyError::Storage)?;
        }
        if tokens.refresh_token.is_empty() {
            return Err(CustodyError::NeedsAuthentication);
        }
        if force || tokens.needs_refresh() {
            let (provider, client) = self.grant_config(oauth_use, registration.as_ref())?;
            let renewed =
                match OAuthClient::new()?.refresh(&provider, &client, &tokens.refresh_token) {
                    Ok(renewed) => renewed.stamp_now(),
                    Err(OAuthError::InvalidGrant) => {
                        // The provider revoked the grant: its access token
                        // must not be handed out again either. Keep the
                        // account, drop the dead grant until the next sign-in.
                        credentials.put(
                            id,
                            &registered_secret(
                                &TokenSet::default(),
                                registration,
                                copied,
                                copy_origin,
                            ),
                        );
                        credentials.try_save().map_err(CustodyError::Storage)?;
                        return Err(CustodyError::NeedsAuthentication);
                    }
                    Err(error) => return Err(error.into()),
                };
            let mut renewed = renewed;
            if renewed.scope.is_none() {
                renewed.scope = tokens.scope;
            }
            tokens = renewed;
            credentials.put(
                id,
                &registered_secret(&tokens, registration, copied, copy_origin),
            );
            credentials.try_save().map_err(CustodyError::Storage)?;
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
        let mut credentials = self.credentials()?;
        let secret = credentials
            .get(id)
            .ok_or(CustodyError::NeedsAuthentication)?;
        let credential = if let Some(cloud) = OAuthCloud::from_type_id(account.type_id.as_str()) {
            let tokens = self.fresh_oauth_tokens(&mut credentials, id, cloud.oauth_use(), force)?;
            HandoffCredential::Bearer {
                token: Zeroizing::new(tokens.access_token.clone()),
                expires_unix: tokens.expires_at(),
            }
        } else if account.type_id.as_str() == ICLOUD_DRIVE_TYPE {
            let Some(Grant::Icloud {
                apple_id,
                session,
                copied,
                copy_origin,
            }) = grant_of(&secret)
            else {
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
            credentials.put(
                id,
                &icloud_secret_with_provenance(&apple_id, &renewed, copied, copy_origin),
            );
            credentials.try_save().map_err(CustodyError::Storage)?;
            HandoffCredential::Session {
                headers: headers.headers,
                expires_unix: headers.expires_unix,
            }
        } else if matches!(account.type_id.as_str(), WEBDAV_TYPE | NEXTCLOUD_TYPE) {
            let password = password_credential(&secret);
            if password.is_empty() {
                return Err(CustodyError::NeedsAuthentication);
            }
            let username = account
                .config
                .get("username")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            HandoffCredential::Basic { username, password }
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
        if is_copied_secret(secret) || (has_grant_tag(secret) && grant_of(secret).is_none()) {
            return;
        }
        let type_id = account.type_id.as_str();
        if let Some(cloud) = OAuthCloud::from_type_id(type_id) {
            let Some(Grant::Oauth2 {
                tokens,
                registration,
                ..
            }) = grant_of(secret)
            else {
                return;
            };
            // Imported custody is a copy of a still-used source grant. Detaching
            // this account must not sign its original application out remotely.
            if registration.is_some() {
                return;
            }
            let Ok((provider, registration)) =
                self.grant_config(cloud.oauth_use(), registration.as_ref())
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
