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
    clippy::set_contains_or_insert,
    clippy::struct_excessive_bools,
    reason = "cryptographic sampling and expiry arithmetic are bounded; public errors are represented by BrokerError"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{AccountId, Secret, SecretVault};

const ITEM_PREFIX: &str = "qos-secret-item:";

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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

impl SecretItem {
    pub fn login(
        title: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        let now = now_unix();
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

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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

    pub fn put(&mut self, item: &mut SecretItem) -> Result<(), BrokerError> {
        item.updated_unix = now_unix();
        let payload =
            Zeroizing::new(serde_json::to_string(item).map_err(|_| BrokerError::CorruptItem)?);
        let key = AccountId::new(format!("{ITEM_PREFIX}{}", item.id.0));
        self.vault.put(
            &key,
            &Secret {
                password: String::new(),
                token: Some(payload.to_string()),
            },
        );
        self.vault.try_save().map_err(BrokerError::Storage)
    }

    pub fn remove(&mut self, id: &SecretId) -> Result<bool, BrokerError> {
        let removed = self
            .vault
            .remove(&AccountId::new(format!("{ITEM_PREFIX}{}", id.0)));
        if removed {
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

/// Non-secret login metadata the credential picker may present for one exact
/// web origin. Secret values and notes are deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialCandidate {
    pub item_id: SecretId,
    pub title: String,
    pub username: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessDecision {
    Deny,
    AllowOnce,
    AllowForSession,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrokerError {
    Denied,
    NotFound,
    OriginMismatch,
    InvalidOrigin,
    CorruptItem,
    Storage(String),
    Randomness,
}

/// Session broker. The UI/portal supplies the decision callback; only a user
/// gesture can create a grant. `AllowOnce` is consumed immediately.
pub struct CredentialBroker {
    store: SecretStore,
    session_grants: BTreeSet<(String, SecretId)>,
}

/// A short-lived clipboard ownership lease. Callers clear the clipboard only
/// when `should_clear` says it still contains the value qOS wrote, avoiding
/// destruction of something the user copied afterward.
pub struct ClipboardLease {
    value: Zeroizing<String>,
    expires: Instant,
}

impl ClipboardLease {
    pub fn new(value: Zeroizing<String>, lifetime: Duration) -> Self {
        Self {
            value,
            expires: Instant::now() + lifetime,
        }
    }
    pub fn expired(&self) -> bool {
        Instant::now() >= self.expires
    }
    pub fn should_clear(&self, clipboard: Option<&str>) -> bool {
        self.expired()
            && clipboard.is_some_and(|current| current.as_bytes() == self.value.as_bytes())
    }
}

impl CredentialBroker {
    pub fn new(store: SecretStore) -> Self {
        Self {
            store,
            session_grants: BTreeSet::new(),
        }
    }

    /// Find logins eligible for an exact origin so an app can offer account
    /// selection without already knowing a vault item ID. This never performs
    /// suffix matching: a look-alike host receives no candidates.
    pub fn candidates_for_origin(
        &self,
        origin: &str,
    ) -> Result<Vec<CredentialCandidate>, BrokerError> {
        let origin = canonical_origin(origin)?;
        Ok(self
            .store
            .list()
            .into_iter()
            .filter(|item| item.kind == SecretItemKind::Login && item.origins.contains(&origin))
            .map(|item| CredentialCandidate {
                item_id: item.id,
                title: item.title,
                username: item.username,
            })
            .collect())
    }

    pub fn request<F>(
        &mut self,
        request: &AccessRequest,
        decide: F,
    ) -> Result<Zeroizing<String>, BrokerError>
    where
        F: FnOnce(&AccessRequest, &SecretItem) -> AccessDecision,
    {
        let item = self
            .store
            .get(&request.item_id)
            .ok_or(BrokerError::NotFound)?;
        if let Some(origin) = &request.origin {
            let origin = canonical_origin(origin)?;
            if !item.origins.contains(&origin) {
                return Err(BrokerError::OriginMismatch);
            }
        }
        let grant = (request.app_id.clone(), request.item_id.clone());
        if !self.session_grants.contains(&grant) {
            match decide(request, &item) {
                AccessDecision::Deny => return Err(BrokerError::Denied),
                AccessDecision::AllowOnce => {}
                AccessDecision::AllowForSession => {
                    self.session_grants.insert(grant);
                }
            }
        }
        Ok(Zeroizing::new(item.password))
    }

    pub fn revoke_app(&mut self, app_id: &str) {
        self.session_grants.retain(|(app, _)| app != app_id);
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
        getrandom::getrandom(&mut bytes).map_err(|_| BrokerError::Randomness)?;
        let value = u16::from_ne_bytes(bytes);
        if value < zone {
            return Ok(usize::from(value % bound));
        }
    }
}

fn canonical_origin(input: &str) -> Result<String, BrokerError> {
    let trimmed = input.trim().to_ascii_lowercase();
    let Some((scheme, remainder)) = trimmed.split_once("://") else {
        return Err(BrokerError::InvalidOrigin);
    };
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if !matches!(scheme, "https" | "http")
        || authority.is_empty()
        || !authority.is_ascii()
        || authority.contains(['/', '@', ' ', '\\'])
    {
        return Err(BrokerError::InvalidOrigin);
    }
    Ok(format!("{scheme}://{authority}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("qos-secret-store-{}", uuid::Uuid::new_v4()))
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
    fn broker_blocks_lookalike_origins_and_remembers_only_session_consent() {
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

    #[test]
    fn broker_discovers_only_login_candidates_for_the_exact_origin() {
        let root = root();
        let mut store = SecretStore::open_in(&root);
        let mut login = SecretItem::login("Bank", "ada@example.com", "pw");
        login.add_origin("https://bank.example/login").unwrap();
        store.put(&mut login).unwrap();

        let mut note = SecretItem::login("Recovery note", "", "private note");
        note.kind = SecretItemKind::SecureNote;
        note.add_origin("https://bank.example").unwrap();
        store.put(&mut note).unwrap();

        let broker = CredentialBroker::new(store);
        assert!(
            broker
                .candidates_for_origin("https://bank.example.evil.test")
                .unwrap()
                .is_empty()
        );
        let candidates = broker
            .candidates_for_origin("https://BANK.EXAMPLE/sign-in?next=home")
            .unwrap();
        assert_eq!(
            candidates,
            vec![CredentialCandidate {
                item_id: login.id,
                title: "Bank".into(),
                username: "ada@example.com".into(),
            }]
        );
        std::fs::remove_dir_all(root).ok();
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

    #[test]
    fn clipboard_lease_never_clears_new_user_content() {
        let expired = ClipboardLease::new(Zeroizing::new("secret".into()), Duration::ZERO);
        assert!(expired.should_clear(Some("secret")));
        assert!(!expired.should_clear(Some("new copy")));
        assert!(!expired.should_clear(None));
    }
}
