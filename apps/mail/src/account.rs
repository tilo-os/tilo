//! Mail's view of the shared multi-account store.
//!
//! ## One shared store, one vault
//! Account *management* in qOS lives in the **Settings app**, not in Mail —
//! the same way display, network, and Bluetooth settings do. Mail only
//! *consumes* the account list to fetch and send. The canonical store is the
//! shared [`accounts`] crate: a single JSON document under
//! `$XDG_CONFIG_HOME/qos/accounts/` holding every account of every type, with
//! credentials kept out of it entirely and sealed in the encrypted
//! [`accounts::SecretVault`].
//!
//! This module is the mail-side adapter over that store. It keeps the
//! ergonomic in-memory shapes the rest of the app works with — [`Account`]
//! (an [`AccountConfig`] plus per-account sidebar state) and [`AccountStore`]
//! (the list plus the active index) — and hydrates them from the shared store
//! + vault ([`AccountStore::load`]). The type-specific connection settings
//! ([`AccountConfig`]) are serialised into each shared account's opaque
//! `config`; the password is read back from the vault, never from the JSON.

use std::path::Path;

use accounts::{Account as StoredAccount, AccountId, AccountStore as SharedStore, AccountTypeId, SecretVault};
use serde::{Deserialize, Serialize};

use crate::model::{AccountConfig, Folder};

/// The account-type id Mail contributes to the shared store.
pub const TYPE_ID: &str = "email";

/// A single configured mail account: its connection settings plus the
/// per-account UI state Mail needs (sidebar expand/collapse).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Stable id (the email at creation time is a fine natural key, but we
    /// keep an explicit field so a later rename doesn't reshuffle the store).
    pub id: String,
    /// Connection + identity settings — exactly what the setup form collects
    /// and the transport layer consumes.
    pub config: AccountConfig,
    /// Whether this account's folder list is collapsed in the sidebar.
    /// Per-account so a user can keep work expanded and personal collapsed.
    #[serde(default)]
    pub collapsed: bool,
    /// A short accent hint (0..8) used to tint the account's avatar disc so
    /// multiple accounts are visually distinct at a glance.
    #[serde(default)]
    pub tint: u8,
}

impl Account {
    pub fn new(config: AccountConfig) -> Self {
        let id = if config.email.is_empty() {
            format!("account-{}", uuid::Uuid::new_v4())
        } else {
            config.email.clone()
        };
        Self { id, config, collapsed: false, tint: 0 }
    }

    /// Display label for the account header: the display name, falling back to
    /// the email, falling back to a generic placeholder.
    pub fn label(&self) -> &str {
        if !self.config.display_name.is_empty() {
            &self.config.display_name
        } else if !self.config.email.is_empty() {
            &self.config.email
        } else {
            "New account"
        }
    }

    /// The opaque, secret-free config payload persisted into a shared
    /// [`StoredAccount`]. The password is `#[serde(skip)]` on [`AccountConfig`],
    /// so it never reaches this JSON — it lives only in the vault.
    fn to_stored(&self) -> StoredAccount {
        let mut stored = StoredAccount::new(AccountId::from(self.id.clone()), TYPE_ID);
        stored.display_name = self.label().to_string();
        stored.tint = self.tint;
        stored.set_config(&StoredConfig { config: self.config.clone(), collapsed: self.collapsed });
        stored
    }

    /// Rebuild a mail account from a shared [`StoredAccount`], reading the
    /// password back from `vault`. `None` when the stored config does not
    /// deserialise (a store written by a newer type version).
    fn from_stored(stored: &StoredAccount, vault: &SecretVault) -> Option<Self> {
        let payload = stored.config_as::<StoredConfig>()?;
        let mut config = payload.config;
        if let Some(secret) = vault.get(&stored.id) {
            config.password = secret.password;
        }
        Some(Self {
            id: stored.id.to_string(),
            config,
            collapsed: payload.collapsed,
            tint: stored.tint,
        })
    }
}

/// The opaque config payload a mail [`Account`] serialises into a shared
/// [`StoredAccount`]. The `#[serde(skip)]` password on [`AccountConfig`] keeps
/// the credential out of this JSON by construction; `collapsed` (per-account
/// UI state) rides along so it survives a reload.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredConfig {
    #[serde(flatten)]
    config: AccountConfig,
    #[serde(default)]
    collapsed: bool,
}

/// Mail's in-memory account list: the accounts plus which one is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountStore {
    pub accounts: Vec<Account>,
    /// Index of the account whose mailbox is currently shown. Clamped on read.
    pub active: usize,
}

impl Default for AccountStore {
    fn default() -> Self {
        Self { accounts: Vec::new(), active: 0 }
    }
}

impl AccountStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn single(config: AccountConfig) -> Self {
        Self { accounts: vec![Account::new(config)], active: 0 }
    }

    /// Load the mail accounts from the shared canonical store, hydrating each
    /// password from the secret vault. An empty store when nothing is
    /// configured — never an error.
    pub fn load() -> Self {
        Self::from_shared(&SharedStore::load(), &SecretVault::open())
    }

    /// Load under an explicit config root — the seam tests use, so they never
    /// touch the real user config directory.
    pub fn load_in(config_root: &Path) -> Self {
        Self::from_shared(&SharedStore::load_in(config_root), &SecretVault::open_in(config_root))
    }

    fn from_shared(shared: &SharedStore, vault: &SecretVault) -> Self {
        let ty = AccountTypeId::from(TYPE_ID);
        let accounts: Vec<Account> = shared
            .list(&ty)
            .into_iter()
            .filter_map(|stored| Account::from_stored(stored, vault))
            .collect();
        // Map the shared store's active-id back to Mail's active-index.
        let active = shared
            .active_id(&ty)
            .and_then(|id| accounts.iter().position(|a| a.id == id.as_str()))
            .unwrap_or(0);
        Self { accounts, active }
    }

    /// Write these accounts into the shared store + vault under an explicit
    /// config root, preserving accounts of other types already there. This is
    /// the write half of the seam (Settings owns the lifecycle; this exists so
    /// the round-trip is testable and so a future in-app persist has one path).
    pub fn persist_in(&self, config_root: &Path) -> std::io::Result<()> {
        let ty = AccountTypeId::from(TYPE_ID);
        let mut shared = SharedStore::load_in(config_root);
        let mut vault = SecretVault::open_in(config_root);
        for acct in &self.accounts {
            let id = AccountId::from(acct.id.clone());
            shared.upsert(acct.to_stored());
            if acct.config.password.is_empty() {
                vault.remove(&id);
            } else {
                vault.put(&id, &accounts::Secret::password(acct.config.password.clone()));
            }
        }
        if let Some(active) = self.accounts.get(self.active) {
            shared.set_active(&ty, &AccountId::from(active.id.clone()));
        }
        shared.save_in(config_root)?;
        vault.save();
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    /// The active account, clamped so an out-of-range `active` index (e.g. from
    /// a store an account was removed from) never panics.
    pub fn active_account(&self) -> Option<&Account> {
        if self.accounts.is_empty() {
            return None;
        }
        let idx = self.active.min(self.accounts.len() - 1);
        self.accounts.get(idx)
    }

    pub fn active_config(&self) -> AccountConfig {
        self.active_account().map(|a| a.config.clone()).unwrap_or_default()
    }

    pub fn set_active(&mut self, idx: usize) {
        if idx < self.accounts.len() {
            self.active = idx;
        }
    }

    /// Add (or replace, by id) an account and make it active.
    pub fn upsert(&mut self, account: Account) {
        if let Some(idx) = self.accounts.iter().position(|a| a.id == account.id) {
            self.accounts[idx] = account;
            self.active = idx;
        } else {
            self.accounts.push(account);
            self.active = self.accounts.len() - 1;
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.accounts.retain(|a| a.id != id);
        if self.active >= self.accounts.len() && !self.accounts.is_empty() {
            self.active = self.accounts.len() - 1;
        }
    }

    pub fn toggle_collapsed(&mut self, idx: usize) {
        if let Some(a) = self.accounts.get_mut(idx) {
            a.collapsed = !a.collapsed;
        }
    }

    /// The set of folders to show for a given account. For now every account
    /// uses the canonical system folder set; a future change can read the live
    /// IMAP folder tree per account (the transport layer already maps each
    /// [`Folder`] to its server mailbox name).
    pub fn folders_for(_account: &Account) -> [Folder; 7] {
        Folder::ALL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(email: &str, name: &str) -> AccountConfig {
        AccountConfig {
            display_name: name.into(),
            email: email.into(),
            username: email.into(),
            imap_host: "imap.example".into(),
            smtp_host: "smtp.example".into(),
            ..Default::default()
        }
    }

    fn temp_root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("qos-mail-accounts-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn upsert_replaces_by_id_not_appends() {
        let mut store = AccountStore::new();
        store.upsert(Account::new(cfg("a@x.example", "A")));
        let mut updated = Account::new(cfg("a@x.example", "A renamed"));
        updated.id = "a@x.example".into();
        store.upsert(updated);
        assert_eq!(store.len(), 1);
        assert_eq!(store.accounts[0].config.display_name, "A renamed");
    }

    #[test]
    fn active_is_clamped_after_remove() {
        let mut store = AccountStore::new();
        store.upsert(Account::new(cfg("a@x.example", "A")));
        store.upsert(Account::new(cfg("b@y.example", "B")));
        store.set_active(1);
        store.remove("b@y.example");
        // active was 1, now only index 0 exists — must clamp, not panic.
        assert_eq!(store.active_account().unwrap().config.email, "a@x.example");
    }

    #[test]
    fn shared_store_round_trips_accounts_and_active() {
        let root = temp_root();
        let mut store = AccountStore::new();
        store.upsert(Account::new(cfg("a@x.example", "A")));
        store.upsert(Account::new(cfg("b@y.example", "B")));
        store.set_active(0);
        store.persist_in(&root).expect("persist");

        let back = AccountStore::load_in(&root);
        assert_eq!(back.len(), 2);
        assert_eq!(back.active_account().unwrap().config.email, "a@x.example");
        assert_eq!(back.accounts[1].config.display_name, "B");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn password_persists_to_the_vault_not_the_store_json() {
        let root = temp_root();
        let mut c = cfg("a@x.example", "A");
        c.password = "hunter2".into();
        let store = AccountStore::single(c);
        store.persist_in(&root).expect("persist");

        // The plaintext store must never carry the password …
        let store_json =
            std::fs::read_to_string(accounts::store_path_in(&root)).expect("store written");
        assert!(!store_json.contains("hunter2"), "password must not reach the shared store JSON");

        // … but a reload recovers it from the vault.
        let back = AccountStore::load_in(&root);
        assert_eq!(back.accounts[0].config.password, "hunter2", "password rehydrates from the vault");
        std::fs::remove_dir_all(&root).ok();
    }
}
