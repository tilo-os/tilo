//! The secret items Passwords keeps (a login, a key, a note), which are the entries of a
//! workspace's vault, and the store a device kept them in before: Passwords converts what
//! it holds into the workspace's vault and takes it out of the store.

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

use crate::{AccountId, Saved, Secret, SecretVault, VaultBackend};

const ITEM_PREFIX: &str = "tilo-secret-item:";
const MAX_ORIGIN_BYTES: usize = 2_048;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretId(pub String);

impl SecretId {
    /// A new entry's id: random, and spelled as a reference to a vault item names it
    /// ([`is_entry_id`]).
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }
}

/// Whether `text` is an entry's id: 32 lowercase hexadecimal digits, or an older entry's, the
/// same spelled as a UUID with its four hyphens. What names a vault entry from outside it (a
/// reference to a vault item) names it by this id.
#[must_use]
pub fn is_entry_id(text: &str) -> bool {
    let hex = |part: &str| {
        part.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    match text.len() {
        32 => hex(text),
        36 => {
            let parts: Vec<&str> = text.split('-').collect();
            parts.iter().map(|part| part.len()).eq([8, 4, 4, 4, 12])
                && parts.iter().all(|part| hex(part))
        }
        _ => false,
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
    /// A list of items from its JSON form (a backup's), read by code the
    /// runtime holds once rather than every app that reads such a list.
    #[inline(never)]
    pub fn list_from_json(bytes: &[u8]) -> serde_json::Result<Vec<Self>> {
        serde_json::from_slice(bytes)
    }

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

pub struct SecretStore {
    vault: SecretVault,
}

impl SecretStore {
    pub fn open() -> Self {
        Self::open_person(None)
    }
    pub fn open_in(root: &std::path::Path) -> Self {
        Self::open_person(Some(root))
    }
    /// The store under `root` (the person's own when `None`); see
    /// [`SecretVault::open_person`].
    pub fn open_person(root: Option<&std::path::Path>) -> Self {
        Self {
            vault: SecretVault::open_person(root),
        }
    }

    /// Finish writing the edits [`Self::put`] and [`Self::remove`] left in
    /// flight: [`Saved::Pending`] while they are still being written.
    ///
    /// # Errors
    ///
    /// The write failed; the edits stay to be written again.
    pub fn flush(&self) -> Result<Saved, BrokerError> {
        self.vault.commit().map_err(BrokerError::Storage)
    }

    /// Describe where the vault encryption key is currently held.
    ///
    /// Callers should surface a local-file fallback honestly rather than
    /// implying that a desktop keyring accepted the credential.
    pub fn backend(&self) -> VaultBackend {
        self.vault.backend()
    }

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
        // A browser write still in flight keeps the edit: undoing it would
        // queue its removal behind the write and lose the item.
        if let Err(error) = self.vault.commit() {
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
        self.vault.remove(&key);
        if let Err(error) = self.vault.commit() {
            self.vault.put(&key, &previous);
            return Err(BrokerError::Storage(error));
        }
        Ok(true)
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
pub enum BrokerError {
    Denied,
    NotFound,
    InvalidOrigin,
    CorruptItem,
    Storage(String),
    Randomness,
}

impl std::fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied => {
                formatter.write_str("the credential belongs to another service or purpose")
            }
            Self::NotFound => formatter.write_str("no such credential is stored"),
            Self::InvalidOrigin => formatter.write_str("the credential's origin is not valid"),
            Self::CorruptItem => formatter.write_str("the stored credential is damaged"),
            Self::Storage(reason) => {
                write!(formatter, "the credential vault is unavailable: {reason}")
            }
            Self::Randomness => formatter.write_str("the system could not provide randomness"),
        }
    }
}

impl std::error::Error for BrokerError {}

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

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tilo-secret-store-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn a_damaged_store_says_so_and_keeps_what_it_holds() {
        let root = root();
        let mut store = SecretStore::open_in(&root);
        let mut item = SecretItem::login("private-title", "private-user", "private-password");
        store.put(&mut item).unwrap();
        let debug = format!("{item:?}");
        assert!(!debug.contains("private-"));
        let sealed = crate::vault_dir_in(&root).join("vault.enc");
        std::fs::write(&sealed, b"damaged existing ciphertext").unwrap();
        let before = std::fs::read(&sealed).unwrap();
        let damaged = SecretStore::open_in(&root);
        assert!(damaged.recovery_error().is_some());
        assert!(damaged.get(&item.id).is_none());
        drop(damaged);
        assert_eq!(std::fs::read(sealed).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_item_writes_preserve_what_the_store_holds() {
        let root = root();
        let mut store = SecretStore::open_in(&root);
        let mut item = SecretItem::login("Mail", "ada", "original secret");
        store.put(&mut item).unwrap();
        let original = item.clone();
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

        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&saved, &path).unwrap();
        let mut other = SecretItem::login("Other", "ada", "other secret");
        store.put(&mut other).unwrap();
        let reopened = SecretStore::open_in(&root);
        assert_eq!(reopened.get(&item.id), Some(original));
        assert!(reopened.get(&added.id).is_none());
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
    fn only_canonical_origins_are_kept() {
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
