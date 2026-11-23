//! What a host kept of a workspace's vault in its own sealed store, before the workspace's
//! vault was the one document every member's device holds.
//!
//! A host sealed two kinds of record under a workspace's folder: the items its members sign in
//! with, under `tilo-vault-item:`, and Passwords' entries, in the store [`SecretStore`] keeps.
//! [`KeptBefore::under`] reads both as the entries the vault document keeps, with the uses each
//! item recorded; [`KeptBefore::forget`] takes them out once they are in the document, so the
//! host keeps no second copy of any.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zeroize::Zeroizing;

use super::{ItemError, ItemId, ItemKind, Place, Sealed, UseRecord};
use crate::{AccountId, SecretItem, SecretStore, SecretVault};

/// The prefix a host's vault item was sealed under.
const PREFIX: &str = "tilo-vault-item:";

/// One item as a host sealed it. What it allowed apart from the workspace's policy is not
/// carried over: the policy's grants decide every use now.
#[derive(Deserialize)]
struct SealedBefore {
    kind: ItemKind,
    places: BTreeSet<Place>,
    #[serde(default)]
    username: String,
    #[serde(default)]
    sent_in: String,
    secret: Zeroizing<String>,
    added_unix: u64,
    #[serde(default)]
    last_used_unix: Option<u64>,
    #[serde(default)]
    uses: Vec<UseRecord>,
}

/// The records a host kept under one workspace's folder, read as vault entries.
pub struct KeptBefore {
    root: PathBuf,
    items: Vec<ItemId>,
    entries: Vec<(SecretItem, Vec<UseRecord>)>,
    store_entries: Vec<crate::SecretId>,
}

impl std::fmt::Debug for KeptBefore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeptBefore")
            .field("root", &self.root)
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

impl KeptBefore {
    /// What the host kept under `root`, the workspace's folder. A record that does not read is
    /// left where it is and said: it is not carried over, and not lost either.
    ///
    /// # Errors
    /// The sealed store under `root` does not open.
    pub fn under(root: &Path) -> Result<Self, ItemError> {
        let vault = SecretVault::open_person(Some(root));
        if let Some(error) = vault.recovery_error() {
            return Err(ItemError::Storage(error.to_owned()));
        }
        let mut items = Vec::new();
        let mut entries = Vec::new();
        for (key, kept) in vault.entries_with_prefix(PREFIX) {
            let Some(id) = key.strip_prefix(PREFIX).and_then(ItemId::parse) else {
                tracing::warn!("a vault item the host kept has no id it reads; it stays");
                continue;
            };
            let Some(sealed) = kept
                .token
                .map(Zeroizing::new)
                .and_then(|json| serde_json::from_str::<SealedBefore>(&json).ok())
            else {
                tracing::warn!(item = %id.as_str(), "a vault item the host kept does not read; it stays");
                continue;
            };
            let SealedBefore {
                kind,
                places,
                username,
                sent_in,
                secret,
                added_unix,
                last_used_unix,
                uses,
            } = sealed;
            let now = Sealed {
                kind,
                places,
                username,
                sent_in,
                account: String::new(),
                secret: secret.to_string(),
                added_unix,
                last_used_unix,
                uses,
            };
            entries.push((now.entry(&id), now.uses.clone()));
            items.push(id);
        }
        let store = SecretStore::open_person(Some(root));
        let store_entries: Vec<SecretItem> = if store.recovery_error().is_some() {
            Vec::new()
        } else {
            store.list()
        };
        let store_ids = store_entries.iter().map(|entry| entry.id.clone()).collect();
        entries.extend(store_entries.into_iter().map(|entry| (entry, Vec::new())));
        Ok(Self {
            root: root.to_owned(),
            items,
            entries,
            store_entries: store_ids,
        })
    }

    /// Whether the host kept nothing here.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries, with the uses each recorded, as the vault document keeps them.
    #[must_use]
    pub fn entries(&self) -> &[(SecretItem, Vec<UseRecord>)] {
        &self.entries
    }

    /// Take every record read out of the host's sealed stores: they are in the workspace's
    /// vault now.
    ///
    /// # Errors
    /// A store was not written.
    pub fn forget(self) -> Result<(), ItemError> {
        let mut vault = SecretVault::open_person(Some(&self.root));
        for id in &self.items {
            vault.remove(&AccountId::new(format!("{PREFIX}{}", id.as_str())));
        }
        vault.try_save().map_err(ItemError::Storage)?;
        if !self.store_entries.is_empty() {
            let mut store = SecretStore::open_person(Some(&self.root));
            for id in &self.store_entries {
                store
                    .remove(id)
                    .map_err(|error| ItemError::Storage(error.to_string()))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Secret;

    /// A host's items and Passwords' entries under one folder read as entries, keep their uses,
    /// and are gone once forgotten; an account's own sign-in beside them stays.
    #[test]
    fn what_a_host_kept_reads_as_entries_and_goes_once_carried_over() {
        let root = std::env::temp_dir().join(format!("tilo-kept-before-{}", uuid::Uuid::new_v4()));
        let id = ItemId::parse(&"a".repeat(32)).unwrap();
        let mut vault = SecretVault::open_in(&root);
        let sealed = serde_json::json!({
            "kind": "login",
            "places": ["imap://mail.example.test:993", "smtp://mail.example.test:465"],
            "username": "ada@example.test",
            "secret": "correct horse battery staple",
            "added_unix": 7,
            "uses": [{"at_unix": 9, "places": ["imap://mail.example.test:993"], "by": ""}],
            "leaves": [{"by": "Assistant", "places": ["imap://mail.example.test:993"]}],
        });
        vault.put(
            &AccountId::new(format!("{PREFIX}{}", id.as_str())),
            &Secret {
                password: String::new(),
                token: Some(sealed.to_string()),
            },
        );
        vault.put(
            &AccountId::new("ada@example.test"),
            &Secret::password("an account's own"),
        );
        vault.try_save().unwrap();
        let mut store = SecretStore::open_in(&root);
        let mut entry = SecretItem::login("Example", "ada", "hunter22");
        entry.add_origin("https://example.test").unwrap();
        store.put(&mut entry).unwrap();

        let kept = KeptBefore::under(&root).unwrap();
        assert_eq!(kept.entries().len(), 2, "{kept:?}");
        let (item, uses) = &kept.entries()[0];
        assert_eq!(item.id.0, id.as_str());
        assert_eq!(item.username, "ada@example.test");
        assert_eq!(item.password, "correct horse battery staple");
        assert_eq!(item.origins.len(), 2);
        assert_eq!(uses.len(), 1);
        assert_eq!(kept.entries()[1].0.id, entry.id);

        kept.forget().unwrap();
        assert!(KeptBefore::under(&root).unwrap().is_empty());
        assert_eq!(
            SecretVault::open_in(&root)
                .get(&AccountId::new("ada@example.test"))
                .map(|secret| secret.password),
            Some("an account's own".to_owned())
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
