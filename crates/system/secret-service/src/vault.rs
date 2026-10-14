//! The vault: everything the store keeps, as one value that is sealed whole.
//!
//! Collections hold items; each item has an owner, the identity that made
//! it. The ledger records every identity that ever used the store, whether
//! it is still allowed, and when it last did. The vault also holds the
//! per-app master secrets the desktop portal hands to sandboxed apps, and
//! the outcome of importing an older keyring.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroize;

use crate::app::{AppId, Trust};

/// The vault layout this build writes.
pub const VAULT_VERSION: u32 = 1;
/// The collection libsecret's `default` alias names.
pub const LOGIN_COLLECTION: &str = "login";
/// The in-memory collection of libsecret's `session` alias.
pub const SESSION_COLLECTION: &str = "session";

/// Secret bytes, wiped when dropped and never printed.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes({} bytes)", self.0.len())
    }
}

impl Serialize for SecretBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Bytes;
        impl<'de> Visitor<'de> for Bytes {
            type Value = SecretBytes;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("secret bytes")
            }
            fn visit_bytes<E: de::Error>(self, value: &[u8]) -> Result<SecretBytes, E> {
                Ok(SecretBytes(value.to_vec()))
            }
            fn visit_byte_buf<E: de::Error>(self, value: Vec<u8>) -> Result<SecretBytes, E> {
                Ok(SecretBytes(value))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<SecretBytes, A::Error> {
                let mut bytes = Vec::new();
                while let Some(byte) = seq.next_element::<u8>()? {
                    bytes.push(byte);
                }
                Ok(SecretBytes(bytes))
            }
        }
        deserializer.deserialize_byte_buf(Bytes)
    }
}

/// Who made an item, and so who may read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Owner {
    /// Made by this identity; only it reads it.
    App(AppId),
    /// Brought over from an older keyring. Apps of the system read it; an
    /// application the person merely runs does not.
    Imported,
}

impl Owner {
    /// Whether `caller` may see an item with this owner.
    #[must_use]
    pub fn admits(&self, caller: &AppId) -> bool {
        match self {
            Self::App(owner) => owner == caller,
            Self::Imported => caller.trust() == Trust::System,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub label: String,
    pub attributes: BTreeMap<String, String>,
    pub secret: SecretBytes,
    pub content_type: String,
    pub created: u64,
    pub modified: u64,
    pub owner: Owner,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub label: String,
    pub created: u64,
    pub modified: u64,
    /// Kept in memory only, like libsecret's `session` collection.
    pub transient: bool,
    /// Keyed by a random 128-bit hexadecimal id: the id is the item's
    /// D-Bus path, and is not guessable.
    pub items: BTreeMap<String, Item>,
}

/// Whether an identity may use the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    Allowed,
    Revoked,
}

/// What the store knows of one application.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRecord {
    pub first_seen: u64,
    pub last_used: u64,
    pub uses: u64,
    pub access: Access,
}

/// The outcome of importing an older keyring.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migration {
    pub source: String,
    pub at: u64,
    pub imported: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vault {
    pub version: u32,
    pub collections: BTreeMap<String, Collection>,
    pub aliases: BTreeMap<String, String>,
    /// Keyed by [`AppId`].
    pub apps: BTreeMap<AppId, AppRecord>,
    /// The portal's per-application master secrets, keyed by the app's
    /// package id.
    pub sandbox_secrets: BTreeMap<String, SecretBytes>,
    pub migration: Option<Migration>,
}

impl Vault {
    /// A new vault: the login collection and the session collection, with
    /// the aliases libsecret expects.
    #[must_use]
    pub fn new(now: u64) -> Self {
        let collection = |label: &str, transient| Collection {
            label: label.to_owned(),
            created: now,
            modified: now,
            transient,
            items: BTreeMap::new(),
        };
        Self {
            version: VAULT_VERSION,
            collections: BTreeMap::from([
                (LOGIN_COLLECTION.to_owned(), collection("Login", false)),
                (SESSION_COLLECTION.to_owned(), collection("Session", true)),
            ]),
            aliases: BTreeMap::from([
                ("default".to_owned(), LOGIN_COLLECTION.to_owned()),
                (SESSION_COLLECTION.to_owned(), SESSION_COLLECTION.to_owned()),
            ]),
            apps: BTreeMap::new(),
            sandbox_secrets: BTreeMap::new(),
            migration: None,
        }
    }

    /// The collection `name_or_alias` names.
    #[must_use]
    pub fn resolve<'a>(&'a self, name_or_alias: &'a str) -> &'a str {
        self.aliases
            .get(name_or_alias)
            .map_or(name_or_alias, String::as_str)
    }

    /// This vault without the collections that live in memory only: the
    /// value that is sealed.
    #[must_use]
    pub fn persistable(&self) -> Self {
        let mut copy = self.clone();
        for collection in copy.collections.values_mut().filter(|c| c.transient) {
            collection.items.clear();
        }
        copy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_items_are_for_the_system_and_own_items_for_their_owner() {
        let system = AppId::parse("binary:/usr/bin/chromium").unwrap();
        let local = AppId::parse("local-process:/home/me/tool").unwrap();
        assert!(Owner::Imported.admits(&system));
        assert!(!Owner::Imported.admits(&local));
        assert!(Owner::App(local.clone()).admits(&local));
        assert!(!Owner::App(local).admits(&system));
    }

    #[test]
    fn the_session_collection_is_never_persisted() {
        let mut vault = Vault::new(1);
        let item = Item {
            label: "x".into(),
            attributes: BTreeMap::new(),
            secret: SecretBytes::new(b"s".to_vec()),
            content_type: "text/plain".into(),
            created: 1,
            modified: 1,
            owner: Owner::Imported,
        };
        for name in [LOGIN_COLLECTION, SESSION_COLLECTION] {
            vault
                .collections
                .get_mut(name)
                .unwrap()
                .items
                .insert("a".into(), item.clone());
        }
        let kept = vault.persistable();
        assert_eq!(kept.collections[LOGIN_COLLECTION].items.len(), 1);
        assert!(kept.collections[SESSION_COLLECTION].items.is_empty());
        assert_eq!(vault.resolve("default"), LOGIN_COLLECTION);
    }

    #[test]
    fn a_vault_survives_postcard_and_debug_hides_secrets() {
        let mut vault = Vault::new(5);
        vault
            .sandbox_secrets
            .insert("so.tilo.notes".into(), SecretBytes::new(vec![7; 32]));
        let bytes = postcard::to_allocvec(&vault).unwrap();
        let back: Vault = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, vault);
        assert!(!format!("{vault:?}").contains("[7, 7"));
    }
}
