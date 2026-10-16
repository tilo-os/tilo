//! General-purpose encrypted secrets and the credential broker.
//!
//! Applications should ask [`CredentialBroker`] for a credential instead of
//! opening [`crate::SecretVault`] themselves. The broker requires an explicit,
//! narrowly-scoped user decision and never returns entries for a look-alike
//! origin. Password-manager metadata is encrypted together with the secret;
//! there is no plaintext index leaking which services a user has accounts on.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::missing_errors_doc,
    clippy::struct_excessive_bools,
    reason = "cryptographic sampling and expiry arithmetic are bounded; public errors are represented by BrokerError"
)]

use std::collections::{BTreeMap, BTreeSet};

use getrandom::{SysRng, rand_core::TryRng as _};
use serde::{Deserialize, Serialize};
use tilo_clock::unix_now;
use zeroize::Zeroizing;

use crate::{AccountId, Secret, SecretCustody, SecretVault};

const ITEM_PREFIX: &str = "tilo-secret-item:";
const GRANT_PREFIX: &str = "tilo-secret-grant:";
const MAX_APP_ID_BYTES: usize = 255;
const MAX_PURPOSE_BYTES: usize = 160;
const MAX_ORIGIN_BYTES: usize = 2_048;

#[path = "credential_owner.rs"]
mod owner;
pub use owner::CredentialOwner;

#[path = "credential_pending.rs"]
mod pending;
pub use pending::CredentialProposal;

#[cfg(test)]
#[path = "credential_owner_tests.rs"]
mod owner_tests;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretId(pub String);

impl SecretId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for SecretId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretItemKind {
    #[default]
    Login,
    ApiKey,
    SecureNote,
    Wifi,
    Other,
}

/// One encrypted vault item. Even title, username, URLs and tags stay inside
/// the authenticated ciphertext because this metadata is often sensitive.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretItem {
    pub id: SecretId,
    pub kind: SecretItemKind,
    pub title: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub origins: BTreeSet<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    #[serde(default)]
    pub tags: BTreeSet<String>,
    pub created_unix: u64,
    pub updated_unix: u64,
}

impl std::fmt::Debug for SecretItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretItem")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl SecretItem {
    pub fn login(
        title: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        let now = unix_now();
        Self {
            id: SecretId::new(),
            kind: SecretItemKind::Login,
            title: title.into(),
            username: username.into(),
            password: password.into(),
            origins: BTreeSet::new(),
            notes: String::new(),
            fields: BTreeMap::new(),
            tags: BTreeSet::new(),
            created_unix: now,
            updated_unix: now,
        }
    }

    pub fn add_origin(&mut self, origin: &str) -> Result<(), BrokerError> {
        self.origins.insert(canonical_origin(origin)?);
        Ok(())
    }
}

/// Typed password-manager view over the shared encrypted container.
pub struct SecretStore {
    vault: SecretVault,
}

impl SecretStore {
    pub fn open() -> Self {
        Self {
            vault: SecretVault::open(),
        }
    }
    pub fn open_in(root: &std::path::Path) -> Self {
        Self {
            vault: SecretVault::open_in(root),
        }
    }

    /// Describe where the vault encryption key is currently held.
    ///
    /// Callers should surface a local-file fallback honestly rather than
    /// implying that a desktop keyring accepted the credential.
    pub fn custody(&self) -> SecretCustody {
        self.vault.custody()
    }

    /// Initial storage failure, if the vault needs recovery before editing.
    pub fn recovery_error(&self) -> Option<&str> {
        self.vault.recovery_error()
    }

    pub fn list(&self) -> Vec<SecretItem> {
        let mut items: Vec<SecretItem> = self
            .vault
            .entries_with_prefix(ITEM_PREFIX)
            .into_iter()
            .filter_map(|(_, entry)| entry.token)
            .filter_map(|json| serde_json::from_str(&json).ok())
            .collect();
        items.sort_by(|a, b| {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        items
    }

    pub fn get(&self, id: &SecretId) -> Option<SecretItem> {
        let key = AccountId::new(format!("{ITEM_PREFIX}{}", id.0));
        self.vault
            .get(&key)?
            .token
            .and_then(|json| serde_json::from_str(&json).ok())
    }
    fn contains(&self, id: &SecretId) -> bool {
        self.vault
            .contains(&AccountId::new(format!("{ITEM_PREFIX}{}", id.0)))
    }

    pub fn put(&mut self, item: &mut SecretItem) -> Result<(), BrokerError> {
        let previous_updated = item.updated_unix;
        item.updated_unix = unix_now();
        let payload =
            Zeroizing::new(serde_json::to_string(item).map_err(|_| BrokerError::CorruptItem)?);
        let key = AccountId::new(format!("{ITEM_PREFIX}{}", item.id.0));
        let previous = self.vault.get(&key);
        self.vault.put(
            &key,
            &Secret {
                password: String::new(),
                token: Some(payload.to_string()),
            },
        );
        if let Err(error) = self.vault.try_save() {
            if let Some(previous) = previous {
                self.vault.put(&key, &previous);
            } else {
                self.vault.remove(&key);
            }
            item.updated_unix = previous_updated;
            return Err(BrokerError::Storage(error));
        }
        Ok(())
    }

    pub fn remove(&mut self, id: &SecretId) -> Result<bool, BrokerError> {
        let key = AccountId::new(format!("{ITEM_PREFIX}{}", id.0));
        let Some(previous) = self.vault.get(&key) else {
            return Ok(false);
        };
        let grants = self
            .grants()
            .into_iter()
            .filter(|grant| grant.item_id == *id)
            .filter_map(|grant| {
                let key = AccountId::new(grant.storage_key());
                self.vault.get(&key).map(|secret| (key, secret))
            })
            .collect::<Vec<_>>();
        self.vault.remove(&key);
        self.remove_grants_matching(|grant| grant.item_id == *id);
        if let Err(error) = self.vault.try_save() {
            self.vault.put(&key, &previous);
            for (key, secret) in grants {
                self.vault.put(&key, &secret);
            }
            return Err(BrokerError::Storage(error));
        }
        Ok(true)
    }

    /// Durable application access, sorted for a stable management UI.
    pub fn grants(&self) -> Vec<CredentialGrant> {
        let mut grants = self
            .vault
            .entries_with_prefix(GRANT_PREFIX)
            .into_iter()
            .filter_map(|(_, entry)| entry.token)
            .filter_map(|json| serde_json::from_str(&json).ok())
            .collect::<Vec<CredentialGrant>>();
        grants.sort_by(|a, b| {
            a.app_id
                .cmp(&b.app_id)
                .then_with(|| a.item_id.cmp(&b.item_id))
                .then_with(|| a.origin.cmp(&b.origin))
        });
        grants
    }

    fn has_persistent_grant(&self, key: &GrantKey) -> bool {
        self.grants().iter().any(|grant| grant.key() == *key)
    }

    fn persist_grant(&mut self, grant: &CredentialGrant) -> Result<(), BrokerError> {
        let payload =
            Zeroizing::new(serde_json::to_string(grant).map_err(|_| BrokerError::CorruptItem)?);
        let key = AccountId::new(grant.storage_key());
        self.vault.put(
            &key,
            &Secret {
                password: String::new(),
                token: Some(payload.to_string()),
            },
        );
        if let Err(error) = self.vault.try_save() {
            // A failed durable decision must not become an in-memory grant for
            // the remainder of this broker process.
            self.vault.remove(&key);
            return Err(BrokerError::Storage(error));
        }
        Ok(())
    }

    fn remove_grants_matching(&mut self, predicate: impl Fn(&CredentialGrant) -> bool) -> usize {
        let matches = self
            .grants()
            .into_iter()
            .filter(predicate)
            .collect::<Vec<_>>();
        for grant in &matches {
            self.vault.remove(&AccountId::new(grant.storage_key()));
        }
        matches.len()
    }

    /// Revoke one exact durable application permission.
    pub fn revoke_grant(&mut self, grant: &CredentialGrant) -> Result<bool, BrokerError> {
        let removed = self.remove_grants_matching(|candidate| candidate == grant) > 0;
        if removed {
            self.vault.try_save().map_err(BrokerError::Storage)?;
        }
        Ok(removed)
    }

    /// Revoke every durable permission held by one application.
    pub fn revoke_app_grants(&mut self, app_id: &str) -> Result<usize, BrokerError> {
        let removed = self.remove_grants_matching(|grant| grant.app_id == app_id);
        if removed > 0 {
            self.vault.try_save().map_err(BrokerError::Storage)?;
        }
        Ok(removed)
    }

    /// Fetch secret material for a user-initiated reveal/copy operation. The
    /// returned buffer clears itself on drop.
    pub fn reveal_password(&self, id: &SecretId) -> Result<Zeroizing<String>, BrokerError> {
        self.get(id)
            .map(|item| Zeroizing::new(item.password))
            .ok_or(BrokerError::NotFound)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessRequest {
    pub app_id: String,
    pub item_id: SecretId,
    pub purpose: String,
    pub origin: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessDecision {
    Deny,
    AllowOnce,
    AllowForSession,
    AllowAlways,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GrantKey {
    app_id: String,
    item_id: SecretId,
    origin: Option<String>,
    scope: Option<CredentialScope>,
}

/// Frozen native resource identity. Metadata alone never admits its caller;
/// the authenticated host validates ownership before requesting this grant.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialScope {
    pub principal: String,
    pub device: String,
    pub service: String,
    pub account: String,
    pub generation: u64,
}
impl CredentialScope {
    fn valid(&self) -> bool {
        self.generation > 0
            && [&self.principal, &self.device, &self.service, &self.account]
                .iter()
                .all(|value| {
                    !value.is_empty()
                        && value.len() <= 512
                        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
                })
    }
}

/// The permission's lifetime. Session permissions never enter the vault file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrantLifetime {
    #[default]
    Persistent,
    Session,
}
fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// A credential permission. Durable relationships are encrypted; session
/// relationships stay in their original broker's memory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialGrant {
    id: String,
    pub app_id: String,
    pub item_id: SecretId,
    pub origin: Option<String>,
    pub created_unix: u64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub lifetime: GrantLifetime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<CredentialScope>,
}

impl CredentialGrant {
    fn new(key: GrantKey) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            app_id: key.app_id,
            item_id: key.item_id,
            origin: key.origin,
            created_unix: unix_now(),
            lifetime: GrantLifetime::Persistent,
            scope: key.scope,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn key(&self) -> GrantKey {
        GrantKey {
            app_id: self.app_id.clone(),
            item_id: self.item_id.clone(),
            origin: self.origin.clone(),
            scope: self.scope.clone(),
        }
    }

    fn storage_key(&self) -> String {
        format!("{GRANT_PREFIX}{}", self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrokerError {
    Denied,
    NotFound,
    OriginMismatch,
    InvalidOrigin,
    InvalidAppId,
    InvalidPurpose,
    CorruptItem,
    Storage(String),
    Randomness,
}

/// Session broker. The UI/portal supplies the decision callback; only a user
/// gesture can create a grant. `AllowOnce` is consumed immediately.
pub struct CredentialBroker {
    store: SecretStore,
    session_grants: BTreeMap<GrantKey, CredentialGrant>,
    revision: u64,
}

impl CredentialBroker {
    pub fn new(store: SecretStore) -> Self {
        Self {
            store,
            session_grants: BTreeMap::new(),
            revision: 0,
        }
    }

    /// Read only under an existing exact item/application/origin grant.
    /// Background account startup and native I/O must use this door: no
    /// permission callback runs and a consumed one-shot grant cannot restart.
    pub fn request_existing(
        &mut self,
        request: &AccessRequest,
    ) -> Result<Zeroizing<String>, BrokerError> {
        self.request(request, |_, _| AccessDecision::Deny)
    }
    /// Read under an existing exact resource grant; legacy unscoped grants do
    /// not authorize this door. No permission callback or implicit grant runs.
    pub fn request_existing_scoped(
        &mut self,
        request: &AccessRequest,
        scope: &CredentialScope,
    ) -> Result<Zeroizing<String>, BrokerError> {
        self.request_scoped(request, scope, |_, _, _| AccessDecision::Deny)
    }

    /// Read a private messaging profile key through its existing exact grant.
    /// Profile custody requires the original system vault; local-file fallback
    /// and ordinary password items never authorize this door. Private metadata
    /// binds the item itself, independently of the permission grant.
    pub fn request_existing_profile_key(
        &mut self,
        request: &AccessRequest,
        scope: &CredentialScope,
    ) -> Result<Zeroizing<String>, BrokerError> {
        if self.store.custody() != SecretCustody::System
            || self.store.recovery_error().is_some()
            || request.app_id != "so.tilo.messages"
            || !scope.valid()
        {
            return Err(BrokerError::Denied);
        }
        let mut item = self
            .store
            .get(&request.item_id)
            .ok_or(BrokerError::NotFound)?;
        // This metadata lookup does not return or retain another plaintext copy.
        zeroize::Zeroize::zeroize(&mut item.password);
        let bound = item.kind == SecretItemKind::Other
            && item.fields.get("tilo.messaging.profile.scope")
                == Some(&serde_json::to_string(scope).map_err(|_| BrokerError::CorruptItem)?);
        if !bound {
            return Err(BrokerError::Denied);
        }
        self.request_existing_scoped(request, scope)
    }

    pub fn request<F>(
        &mut self,
        request: &AccessRequest,
        decide: F,
    ) -> Result<Zeroizing<String>, BrokerError>
    where
        F: FnOnce(&AccessRequest, &SecretItem) -> AccessDecision,
    {
        self.request_in_scope(request, None, decide)
    }
    /// Request one native resource scope through the ordinary user decision door.
    pub fn request_scoped<F>(
        &mut self,
        request: &AccessRequest,
        scope: &CredentialScope,
        decide: F,
    ) -> Result<Zeroizing<String>, BrokerError>
    where
        F: FnOnce(&AccessRequest, &SecretItem, &CredentialScope) -> AccessDecision,
    {
        if !scope.valid() {
            return Err(BrokerError::Denied);
        }
        self.request_in_scope(request, Some(scope.clone()), |request, item| {
            decide(request, item, scope)
        })
    }
    fn request_in_scope<F>(
        &mut self,
        request: &AccessRequest,
        scope: Option<CredentialScope>,
        decide: F,
    ) -> Result<Zeroizing<String>, BrokerError>
    where
        F: FnOnce(&AccessRequest, &SecretItem) -> AccessDecision,
    {
        validate_app_id(&request.app_id)?;
        validate_purpose(&request.purpose)?;
        if let Some(error) = self.store.recovery_error() {
            return Err(BrokerError::Storage(error.to_owned()));
        }
        let item = self
            .store
            .get(&request.item_id)
            .ok_or(BrokerError::NotFound)?;
        let origin = request
            .origin
            .as_deref()
            .map(canonical_origin)
            .transpose()?;
        if let Some(origin) = &origin
            && !item.origins.contains(origin.as_str())
        {
            return Err(BrokerError::OriginMismatch);
        }
        let grant = GrantKey {
            app_id: request.app_id.clone(),
            item_id: request.item_id.clone(),
            origin,
            scope,
        };
        if !self.session_grants.contains_key(&grant) && !self.store.has_persistent_grant(&grant) {
            match decide(request, &item) {
                AccessDecision::Deny => return Err(BrokerError::Denied),
                AccessDecision::AllowOnce => {}
                AccessDecision::AllowForSession => {
                    let mut permission = CredentialGrant::new(grant.clone());
                    permission.lifetime = GrantLifetime::Session;
                    self.session_grants.insert(grant, permission);
                }
                AccessDecision::AllowAlways => {
                    self.store.persist_grant(&CredentialGrant::new(grant))?;
                }
            }
            self.revision = self.revision.saturating_add(1);
        }
        Ok(Zeroizing::new(item.password))
    }

    /// List durable permissions for the Passwords app's access-management UI.
    /// Session and one-shot decisions intentionally disappear with the broker.
    pub fn persistent_grants(&self) -> Vec<CredentialGrant> {
        self.store.grants()
    }

    /// Current exact relationships, including permissions for this session.
    /// The IDs stay stable and no credential values are returned.
    pub fn current_grants(&self) -> Vec<CredentialGrant> {
        let mut grants: BTreeMap<_, _> = self
            .store
            .grants()
            .into_iter()
            .map(|grant| (grant.key(), grant))
            .collect();
        for (key, permission) in &self.session_grants {
            grants
                .entry(key.clone())
                .or_insert_with(|| permission.clone());
        }
        grants.into_values().collect()
    }

    /// Revoke every session and durable permission held by an application.
    pub fn revoke_app(&mut self, app_id: &str) -> Result<usize, BrokerError> {
        let count = self
            .current_grants()
            .iter()
            .filter(|grant| grant.app_id == app_id)
            .count();
        self.session_grants
            .retain(|grant, _| grant.app_id != app_id);
        self.revision = self.revision.saturating_add(1);
        self.store.revoke_app_grants(app_id)?;
        Ok(count)
    }

    /// Revoke one exact durable permission without affecting another account
    /// or another origin used by the same application.
    pub fn revoke_grant(&mut self, grant: &CredentialGrant) -> Result<bool, BrokerError> {
        let key = grant.key();
        if self
            .session_grants
            .get(&key)
            .is_none_or(|current| current.id != grant.id)
            && !self.store.grants().iter().any(|current| current == grant)
        {
            return Ok(false);
        }
        let session = self.session_grants.remove(&key).is_some();
        let durable = self
            .store
            .remove_grants_matching(|candidate| candidate.key() == key)
            > 0;
        self.revision = self.revision.saturating_add(1);
        if durable {
            self.store.vault.try_save().map_err(BrokerError::Storage)?;
        }
        Ok(session || durable)
    }
}

/// Conservative generator defaults: 20 characters and every selected class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeneratedPasswordPolicy {
    pub length: usize,
    pub lowercase: bool,
    pub uppercase: bool,
    pub digits: bool,
    pub symbols: bool,
}

impl Default for GeneratedPasswordPolicy {
    fn default() -> Self {
        Self {
            length: 20,
            lowercase: true,
            uppercase: true,
            digits: true,
            symbols: true,
        }
    }
}

impl GeneratedPasswordPolicy {
    pub fn generate(self) -> Result<Zeroizing<String>, BrokerError> {
        let mut alphabet = String::new();
        let mut required = Vec::new();
        if self.lowercase {
            required.push("abcdefghijkmnopqrstuvwxyz");
        }
        if self.uppercase {
            required.push("ABCDEFGHJKLMNPQRSTUVWXYZ");
        }
        if self.digits {
            required.push("23456789");
        }
        if self.symbols {
            required.push("!@#$%^&*-_=+");
        }
        for class in &required {
            alphabet.push_str(class);
        }
        if self.length < 12 || required.is_empty() || self.length < required.len() {
            return Err(BrokerError::Randomness);
        }
        let mut output = Vec::with_capacity(self.length);
        for class in required {
            output.push(random_from(class.as_bytes())?);
        }
        while output.len() < self.length {
            output.push(random_from(alphabet.as_bytes())?);
        }
        // Fisher-Yates keeps the guaranteed class characters from occupying
        // predictable positions.
        for index in (1..output.len()).rev() {
            let swap = random_index(index + 1)?;
            output.swap(index, swap);
        }
        Ok(Zeroizing::new(
            String::from_utf8(output).map_err(|_| BrokerError::Randomness)?,
        ))
    }
}

fn random_from(alphabet: &[u8]) -> Result<u8, BrokerError> {
    alphabet
        .get(random_index(alphabet.len())?)
        .copied()
        .ok_or(BrokerError::Randomness)
}

fn random_index(bound: usize) -> Result<usize, BrokerError> {
    let bound = u16::try_from(bound).map_err(|_| BrokerError::Randomness)?;
    let zone = u16::MAX - (u16::MAX % bound);
    loop {
        let mut bytes = [0u8; 2];
        SysRng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| BrokerError::Randomness)?;
        let value = u16::from_ne_bytes(bytes);
        if value < zone {
            return Ok(usize::from(value % bound));
        }
    }
}

pub(crate) fn canonical_origin(input: &str) -> Result<String, BrokerError> {
    let trimmed = input.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_ORIGIN_BYTES
        || trimmed.chars().any(char::is_control)
    {
        return Err(BrokerError::InvalidOrigin);
    }
    let parsed = url::Url::parse(trimmed).map_err(|_| BrokerError::InvalidOrigin)?;
    if !matches!(parsed.scheme(), "https" | "http")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(BrokerError::InvalidOrigin);
    }
    Ok(parsed.origin().ascii_serialization())
}

fn validate_app_id(app_id: &str) -> Result<(), BrokerError> {
    if app_id.is_empty()
        || app_id.len() > MAX_APP_ID_BYTES
        || !app_id.is_ascii()
        || app_id.starts_with('.')
        || app_id.ends_with('.')
        || app_id.split('.').any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        return Err(BrokerError::InvalidAppId);
    }
    Ok(())
}

fn validate_purpose(purpose: &str) -> Result<(), BrokerError> {
    let purpose = purpose.trim();
    if purpose.is_empty()
        || purpose.len() > MAX_PURPOSE_BYTES
        || purpose.chars().any(char::is_control)
    {
        return Err(BrokerError::InvalidPurpose);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tilo-secret-store-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn broker_reports_custody_failure_without_prompting_or_hiding_existing_data() {
        let root = root();
        let mut store = SecretStore::open_in(&root);
        let mut item = SecretItem::login("private-title", "private-user", "private-password");
        store.put(&mut item).unwrap();
        let debug = format!("{item:?}");
        assert!(!debug.contains("private-"));
        let sealed = crate::vault_dir_in(&root).join("vault.enc");
        std::fs::write(&sealed, b"damaged existing ciphertext").unwrap();
        let before = std::fs::read(&sealed).unwrap();
        let mut broker = CredentialBroker::new(SecretStore::open_in(&root));
        let request = AccessRequest {
            app_id: "org.tilo.Browser".into(),
            item_id: item.id,
            purpose: "Sign in".into(),
            origin: None,
        };
        assert!(matches!(
            broker.request_existing(&request),
            Err(BrokerError::Storage(_))
        ));
        assert!(matches!(
            broker.request(&request, |_, _| panic!("damaged custody must not prompt")),
            Err(BrokerError::Storage(_))
        ));
        assert_eq!(std::fs::read(sealed).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_item_writes_preserve_credentials_and_permissions() {
        let root = root();
        let mut store = SecretStore::open_in(&root);
        let mut item = SecretItem::login("Mail", "ada", "original secret");
        store.put(&mut item).unwrap();
        let original = item.clone();
        let mut broker = CredentialBroker::new(store);
        broker
            .request(
                &AccessRequest {
                    app_id: "org.example.Mail".into(),
                    item_id: item.id.clone(),
                    purpose: "Sign in".into(),
                    origin: None,
                },
                |_, _| AccessDecision::AllowAlways,
            )
            .unwrap();
        let mut store = broker.store;
        let grants = store.grants();
        assert_eq!(grants.len(), 1);
        let path = crate::vault_dir_in(&root).join("vault.enc");
        let saved = path.with_extension("saved");
        std::fs::rename(&path, &saved).unwrap();
        std::fs::create_dir(&path).unwrap();

        item.password = "unsaved replacement".into();
        item.updated_unix = 1;
        assert!(matches!(store.put(&mut item), Err(BrokerError::Storage(_))));
        assert_eq!(item.updated_unix, 1);
        assert_eq!(store.get(&item.id), Some(original.clone()));
        let mut added = SecretItem::login("Unsaved", "ada", "new secret");
        assert!(matches!(
            store.put(&mut added),
            Err(BrokerError::Storage(_))
        ));
        assert!(store.get(&added.id).is_none());
        assert!(matches!(
            store.remove(&item.id),
            Err(BrokerError::Storage(_))
        ));
        assert_eq!(store.get(&item.id), Some(original.clone()));
        assert_eq!(store.grants(), grants);

        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&saved, &path).unwrap();
        let mut other = SecretItem::login("Other", "ada", "other secret");
        store.put(&mut other).unwrap();
        let reopened = SecretStore::open_in(&root);
        assert_eq!(reopened.get(&item.id), Some(original));
        assert!(reopened.get(&added.id).is_none());
        assert_eq!(reopened.grants(), grants);
        assert_eq!(reopened.get(&other.id), Some(other));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_and_password_are_encrypted() {
        let root = root();
        let mut store = SecretStore::open_in(&root);
        let mut item = SecretItem::login("Very Secret Bank", "ada", "correct horse battery staple");
        item.add_origin("https://BANK.EXAMPLE/login").unwrap();
        assert!(item.origins.contains("https://bank.example"));
        store.put(&mut item).unwrap();
        let raw = std::fs::read(crate::vault_dir_in(&root).join("vault.enc")).unwrap();
        assert!(
            !raw.windows(b"Very Secret Bank".len())
                .any(|w| w == b"Very Secret Bank")
        );
        assert_eq!(
            SecretStore::open_in(&root).get(&item.id).unwrap().username,
            "ada"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn broker_consent_is_scoped_to_the_exact_origin_and_revocable() {
        // Phase `broker_blocks_lookalike_origins_and_remembers_only_session_consent`.
        {
            let root = root();
            let mut store = SecretStore::open_in(&root);
            let mut item = SecretItem::login("Bank", "ada", "pw");
            item.add_origin("https://bank.example").unwrap();
            store.put(&mut item).unwrap();
            let mut broker = CredentialBroker::new(store);
            let bad = AccessRequest {
                app_id: "browser".into(),
                item_id: item.id.clone(),
                purpose: "Sign in".into(),
                origin: Some("https://bank.example.evil.test".into()),
            };
            assert_eq!(
                broker.request(&bad, |_, _| AccessDecision::AllowForSession),
                Err(BrokerError::OriginMismatch)
            );
            let good = AccessRequest {
                origin: Some("https://bank.example".into()),
                ..bad
            };
            let mut prompts = 0;
            assert_eq!(
                broker
                    .request(&good, |_, _| {
                        prompts += 1;
                        AccessDecision::AllowForSession
                    })
                    .unwrap()
                    .as_str(),
                "pw"
            );
            assert_eq!(
                broker
                    .request(&good, |_, _| {
                        prompts += 1;
                        AccessDecision::Deny
                    })
                    .unwrap()
                    .as_str(),
                "pw"
            );
            assert_eq!(prompts, 1);
            std::fs::remove_dir_all(root).ok();
        }

        // Phase `durable_consent_survives_restart_but_is_scoped_to_the_exact_origin`.
        {
            let root = root();
            let mut store = SecretStore::open_in(&root);
            let mut item = SecretItem::login("Bank", "ada", "pw");
            item.add_origin("https://bank.example").unwrap();
            item.add_origin("https://accounts.bank.example").unwrap();
            store.put(&mut item).unwrap();
            let bank = AccessRequest {
                app_id: "org.tilo.Browser".into(),
                item_id: item.id.clone(),
                purpose: "Sign in".into(),
                origin: Some("https://bank.example/login".into()),
            };

            CredentialBroker::new(store)
                .request(&bank, |_, _| AccessDecision::AllowAlways)
                .unwrap();
            let mut restarted = CredentialBroker::new(SecretStore::open_in(&root));
            let mut prompts = 0;
            assert_eq!(
                restarted
                    .request(&bank, |_, _| {
                        prompts += 1;
                        AccessDecision::Deny
                    })
                    .unwrap()
                    .as_str(),
                "pw"
            );
            let other_origin = AccessRequest {
                origin: Some("https://accounts.bank.example".into()),
                ..bank
            };
            assert_eq!(
                restarted.request(&other_origin, |_, _| {
                    prompts += 1;
                    AccessDecision::Deny
                }),
                Err(BrokerError::Denied)
            );
            assert_eq!(prompts, 1, "only the differently scoped origin prompts");
            assert_eq!(restarted.persistent_grants().len(), 1);
            std::fs::remove_dir_all(root).ok();
        }

        // Phase `durable_access_can_be_inspected_and_revoked_by_app_or_item`.
        {
            let root = root();
            let mut store = SecretStore::open_in(&root);
            let mut item = SecretItem::login("Bank", "ada", "pw");
            item.add_origin("https://bank.example").unwrap();
            store.put(&mut item).unwrap();
            let request = AccessRequest {
                app_id: "org.tilo.Browser".into(),
                item_id: item.id.clone(),
                purpose: "Sign in".into(),
                origin: Some("https://bank.example".into()),
            };
            let mut broker = CredentialBroker::new(store);
            broker
                .request(&request, |_, _| AccessDecision::AllowAlways)
                .unwrap();
            let grant = broker.persistent_grants().pop().unwrap();
            assert_eq!(grant.app_id, "org.tilo.Browser");
            assert_eq!(grant.origin.as_deref(), Some("https://bank.example"));
            assert!(broker.revoke_grant(&grant).unwrap());

            broker
                .request(&request, |_, _| AccessDecision::AllowAlways)
                .unwrap();
            assert_eq!(broker.revoke_app("org.tilo.Browser").unwrap(), 1);
            assert!(broker.persistent_grants().is_empty());

            broker
                .request(&request, |_, _| AccessDecision::AllowAlways)
                .unwrap();
            drop(broker);
            let mut store = SecretStore::open_in(&root);
            assert!(store.remove(&item.id).unwrap());
            assert!(
                CredentialBroker::new(SecretStore::open_in(&root))
                    .persistent_grants()
                    .is_empty(),
                "deleting an item cannot leave an orphaned app permission"
            );
            std::fs::remove_dir_all(root).ok();
        }
    }

    #[test]
    fn broker_accepts_only_canonical_origins_and_trusted_prompt_metadata() {
        // Phase `origins_use_web_origin_rules_and_reject_credential_or_control_injection`.
        {
            assert_eq!(
                canonical_origin(" HTTPS://BÜCHER.example:443/path?q=1 ").unwrap(),
                "https://xn--bcher-kva.example"
            );
            assert_eq!(
                canonical_origin("http://example.com:8080/a").unwrap(),
                "http://example.com:8080"
            );
            for invalid in [
                "https://user:password@example.com",
                "https://example.com\nspoofed",
                "file:///etc/passwd",
                "https://",
            ] {
                assert_eq!(canonical_origin(invalid), Err(BrokerError::InvalidOrigin));
            }
        }

        // Phase `broker_rejects_untrusted_prompt_metadata_before_consulting_the_user`.
        {
            let root = root();
            let mut store = SecretStore::open_in(&root);
            let mut item = SecretItem::login("Bank", "ada", "pw");
            store.put(&mut item).unwrap();
            let mut broker = CredentialBroker::new(store);
            let mut prompted = false;
            let invalid_app = AccessRequest {
                app_id: "org.tilo.Browser\nFake".into(),
                item_id: item.id.clone(),
                purpose: "Sign in".into(),
                origin: None,
            };
            assert_eq!(
                broker.request(&invalid_app, |_, _| {
                    prompted = true;
                    AccessDecision::AllowAlways
                }),
                Err(BrokerError::InvalidAppId)
            );
            assert!(!prompted);

            let invalid_purpose = AccessRequest {
                app_id: "org.tilo.Browser".into(),
                purpose: "Sign in\nApprove something else".into(),
                ..invalid_app
            };
            assert_eq!(
                broker.request(&invalid_purpose, |_, _| AccessDecision::AllowAlways),
                Err(BrokerError::InvalidPurpose)
            );
            assert!(broker.persistent_grants().is_empty());
            std::fs::remove_dir_all(root).ok();
        }
    }

    #[test]
    fn generated_password_is_long_and_avoids_ambiguous_characters() {
        let password = GeneratedPasswordPolicy::default().generate().unwrap();
        assert_eq!(password.len(), 20);
        assert!(!password.contains(['0', '1', 'I', 'l', 'O']));
        assert!(password.chars().any(|c| c.is_ascii_lowercase()));
        assert!(password.chars().any(|c| c.is_ascii_uppercase()));
        assert!(password.chars().any(|c| c.is_ascii_digit()));
        assert!(password.chars().any(|c| "!@#$%^&*-_=+".contains(c)));
    }
}
