//! Mail's view of the shared multi-account store.
//!
//! Account management lives in Settings, not in Mail, which only consumes the account list to
//! fetch and send. The canonical store is the shared [`accounts`] crate: one JSON document
//! under `$XDG_CONFIG_HOME/tilo/accounts/` holding every account of every type, with
//! credentials kept out of it and sealed in the encrypted
//! [`accounts::AccountCredentials`].
//!
//! This module keeps the shapes the rest of Mail works with, [`Account`] and
//! [`AccountStore`], and reads them from that store ([`AccountStore::load_in`]). The connection
//! settings are serialised into each account's opaque `config`. A password is never read here:
//! the session releases it from the workspace's vault to whoever signs in
//! (`accounts::sign_ins`).

use std::path::Path;

use accounts::person::PersonAccounts;
use accounts::{
    Account as StoredAccount, AccountCredentials, AccountId, AccountStore as SharedStore,
    AccountTypeId,
};
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{Id, Invoke};

use crate::model::{AccountConfig, Folder};

pub const TYPE_ID: &str = accounts::email::TYPE_ID;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub config: AccountConfig,
    /// Whether this account's folder list is collapsed in the sidebar.
    #[serde(skip)]
    pub collapsed: bool,
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
        Self {
            id,
            config,
            collapsed: false,
            tint: 0,
        }
    }

    fn to_stored(&self) -> StoredAccount {
        let type_id = if self.config.import_pending {
            accounts::thunderbird::PENDING_TYPE
        } else {
            TYPE_ID
        };
        let mut stored = StoredAccount::new(AccountId::from(self.id.clone()), type_id);
        stored.display_name = self.config.label().to_string();
        stored.tint = self.tint;
        stored.set_config(&StoredConfig {
            config: self.config.clone(),
        });
        stored
    }

    fn from_stored(stored: &StoredAccount) -> Option<Self> {
        let config = config_of(stored)?;
        Some(Self {
            id: stored.id.to_string(),
            config,
            collapsed: false,
            tint: stored.tint,
        })
    }
}

/// The mail settings `stored` keeps, without a password.
#[must_use]
pub fn config_of(stored: &StoredAccount) -> Option<AccountConfig> {
    let mut config = stored.config_as::<StoredConfig>()?.config;
    config.import_pending |= stored.type_id.as_str() == accounts::thunderbird::PENDING_TYPE;
    Some(config)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredConfig {
    #[serde(flatten)]
    config: AccountConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AccountStore {
    pub accounts: Vec<Account>,
    pub active: usize,
}

impl AccountStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn single(config: AccountConfig) -> Self {
        Self {
            accounts: vec![Account::new(config)],
            active: 0,
        }
    }

    /// The mail accounts the store under `config_root` keeps, without a password.
    pub fn load_in(config_root: &Path) -> Self {
        Self::from_shared(&SharedStore::load_in(config_root))
    }

    /// The mail accounts `shared` keeps, without a password.
    pub fn from_shared(shared: &SharedStore) -> Self {
        let ty = AccountTypeId::from(TYPE_ID);
        let accounts: Vec<Account> = shared
            .list(&ty)
            .into_iter()
            .chain(shared.list(&accounts::thunderbird::PENDING_TYPE.into()))
            .filter_map(Account::from_stored)
            .collect();
        let active = shared
            .active_id(&ty)
            .and_then(|id| accounts.iter().position(|a| a.id == id.as_str()))
            .unwrap_or(0);
        Self { accounts, active }
    }

    /// Write these accounts into the shared store + vault under an explicit
    /// config root, preserving accounts of other types already there. This is
    /// the full write path: it upserts the current accounts, **reconciles
    /// removals** (an email account deleted in Mail is dropped from the shared
    /// store and its vault secret purged), and records the active selection.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the shared account store or the secret vault cannot
    /// be written under `config_root`.
    pub fn persist_in(&self, config_root: &Path) -> std::io::Result<()> {
        self.persist_with_vault(
            config_root,
            AccountCredentials::open_in(config_root).map_err(std::io::Error::other)?,
        )
    }

    fn persist_with_vault(
        &self,
        config_root: &Path,
        mut vault: AccountCredentials,
    ) -> std::io::Result<()> {
        let ty = AccountTypeId::from(TYPE_ID);
        let mut shared = SharedStore::load_in(config_root);

        // Reconcile deletions: any email account in the shared store that Mail
        // no longer holds was removed by the user — drop it and its secret.
        let current: std::collections::HashSet<String> =
            self.accounts.iter().map(|a| a.id.clone()).collect();
        let stale: Vec<AccountId> = shared
            .list(&ty)
            .into_iter()
            .map(|s| s.id.clone())
            .filter(|id| !current.contains(id.as_str()))
            .collect();
        for id in stale {
            shared.remove(&id);
            vault.remove(&id);
        }

        for acct in &self.accounts {
            let id = AccountId::from(acct.id.clone());
            shared.upsert(acct.to_stored());
            if acct.config.import_pending {
                continue;
            }
            // Keep an existing sealed OAuth token intact: an OAuth account has
            // no password, and its token was sealed by the sign-in flow — do
            // not clobber it. Only manage the password secret here.
            if acct.config.password.is_empty() {
                if acct.config.auth != crate::model::AuthMethod::OAuth {
                    vault.remove(&id);
                }
            } else {
                vault.put_for(
                    &acct.to_stored(),
                    &accounts::Secret::password(acct.config.password.clone()),
                );
            }
        }
        if let Some(active) = self.accounts.get(self.active) {
            shared.set_active(&ty, &AccountId::from(active.id.clone()));
        }
        vault.try_save().map_err(std::io::Error::other)?;
        shared.save_in(config_root)
    }

    /// The edits that make the workspace's accounts hold these mail accounts.
    pub fn edits_for(
        &self,
        person: &PersonAccounts,
    ) -> (Vec<Invoke<PersonAccounts>>, Vec<AccountId>) {
        let ty = AccountTypeId::from(TYPE_ID);
        let held = person.store();
        let mut edits = Vec::new();
        let mut removed = Vec::new();
        for stale in held.list(&ty) {
            if !self.accounts.iter().any(|a| a.id == stale.id.as_str()) {
                edits.push(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw(stale.id.0.clone())));
                removed.push(stale.id.clone());
            }
        }
        for account in &self.accounts {
            let mut stored = account.to_stored();
            // The vault entry its password is in is the workspace's to name, which Mail's
            // settings of the account do not carry: it stays as the record names it.
            if let Some(item) = held
                .get(&stored.id)
                .and_then(|kept| accounts::email::password_item(&kept.config))
            {
                accounts::email::set_password_item(&mut stored.config, Some(item));
            }
            if held.get(&stored.id) != Some(&stored) {
                edits.push(PersonAccounts::saving(&stored));
            }
        }
        (edits, removed)
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn active_account(&self) -> Option<&Account> {
        if self.accounts.is_empty() {
            return None;
        }
        let idx = self.active.min(self.accounts.len() - 1);
        self.accounts.get(idx)
    }

    pub fn active_config(&self) -> AccountConfig {
        self.active_account()
            .map(|a| a.config.clone())
            .unwrap_or_default()
    }

    /// The name the person knows the account `id` by: its display name, or
    /// its address. `None` for an account this window no longer holds.
    pub fn label_for(&self, id: &str) -> Option<String> {
        self.accounts
            .iter()
            .find(|account| account.id == id || account.config.email == id)
            .map(|account| account.config.label().to_owned())
    }

    pub fn set_active(&mut self, idx: usize) {
        if idx < self.accounts.len() {
            self.active = idx;
        }
    }

    pub fn upsert(&mut self, account: Account) {
        if let Some(idx) = self.accounts.iter().position(|a| a.id == account.id) {
            if let Some(slot) = self.accounts.get_mut(idx) {
                *slot = account;
            }
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

    pub fn folders_for(_account: &Account) -> [Folder; 7] {
        Folder::ALL
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accounts::SecretVault;

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

    tilo_test_env::scenario! {
        /// The account store over the shared store and the vault; each case has a root of its own.
        fn the_account_store_keeps_accounts_and_their_secrets;

        phase account_store_upserts_clamps_and_round_trips_the_shared_store;
        {
            {
                let mut store = AccountStore::new();
                store.upsert(Account::new(cfg("a@x.example", "A")));
                let mut updated = Account::new(cfg("a@x.example", "A renamed"));
                updated.id = "a@x.example".into();
                store.upsert(updated);
                assert_eq!(store.len(), 1);
                assert_eq!(store.accounts[0].config.display_name, "A renamed");
            }

            {
                let mut store = AccountStore::new();
                store.upsert(Account::new(cfg("a@x.example", "A")));
                store.upsert(Account::new(cfg("b@y.example", "B")));
                store.set_active(1);
                store.remove("b@y.example");
                assert_eq!(store.active_account().unwrap().config.email, "a@x.example");
            }

            {
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

            {
                let root = temp_root();
                let mut store = AccountStore::new();
                store.upsert(Account::new(cfg("a@x.example", "A")));
                store.upsert(Account::new(cfg("b@y.example", "B")));
                store.persist_in(&root).expect("persist both");
                assert_eq!(AccountStore::load_in(&root).len(), 2);

                // Remove one and persist again — the shared store must drop it too, so
                // a reload doesn't resurrect the removed account.
                store.remove("a@x.example");
                store.persist_in(&root).expect("persist after remove");
                let back = AccountStore::load_in(&root);
                assert_eq!(back.len(), 1);
                assert_eq!(back.accounts[0].config.email, "b@y.example");
                std::fs::remove_dir_all(&root).ok();
            }
        }

        phase account_secrets_live_in_the_vault_and_a_damaged_vault_blocks_changes;
        {
            {
                let root = temp_root();
                let mut c = cfg("a@x.example", "A");
                c.password = "top-secret".into();
                let mut store = AccountStore::single(c);
                store.persist_in(&root).expect("persist");
                assert!(
                    SecretVault::open_in(&root)
                        .get(&AccountId::from("a@x.example"))
                        .is_some()
                );

                store.remove("a@x.example");
                store.persist_in(&root).expect("persist after remove");
                assert!(
                    SecretVault::open_in(&root)
                        .get(&AccountId::from("a@x.example"))
                        .is_none(),
                    "the removed account's secret must be purged from the vault"
                );
                std::fs::remove_dir_all(&root).ok();
            }

            {
                let root = temp_root();
                let id = AccountId::from("g@gmail.com");
                {
                    let mut vault = SecretVault::open_in(&root);
                    vault.put(
                        &id,
                        &accounts::Secret {
                            password: String::new(),
                            token: Some("{\"access_token\":\"tok\"}".into()),
                        },
                    );
                    vault.save();
                }
                let mut c = cfg("g@gmail.com", "G");
                c.auth = crate::model::AuthMethod::OAuth;
                c.password.clear();
                let store = AccountStore::single(c);
                store.persist_in(&root).expect("persist");
                let secret = SecretVault::open_in(&root)
                    .get(&id)
                    .expect("token survives");
                assert_eq!(secret.token.as_deref(), Some("{\"access_token\":\"tok\"}"));
                std::fs::remove_dir_all(&root).ok();
            }

            {
                let root = temp_root();
                let mut c = cfg("a@x.example", "A");
                c.password = "hunter2".into();
                let store = AccountStore::single(c);
                store.persist_in(&root).expect("persist");

                let store_json =
                    std::fs::read_to_string(accounts::store_path_in(&root)).expect("store written");
                assert!(
                    !store_json.contains("hunter2"),
                    "password must not reach the shared store JSON"
                );

                let back = AccountStore::load_in(&root);
                assert!(
                    back.accounts[0].config.password.is_empty(),
                    "Mail reads no password from the device: the session releases it"
                );
                std::fs::remove_dir_all(&root).ok();
            }

            {
                let root = tempfile::tempdir().unwrap();
                let mut config = cfg("a@x.example", "Original");
                config.password = "fixture-original-secret".into();
                let mut store = AccountStore::single(config);
                store.persist_in(root.path()).unwrap();
                let metadata_path = accounts::store_path_in(root.path());
                let original_metadata = std::fs::read(&metadata_path).unwrap();
                let vault_path = accounts::vault_dir_in(root.path()).join("vault.enc");
                let original_vault = std::fs::read(&vault_path).unwrap();
                let damaged = b"damaged fixture vault";
                std::fs::write(&vault_path, damaged).unwrap();
                store.accounts[0].config.display_name = "Unsaved change".into();
                store.accounts[0].config.password = "fixture-new-secret".into();

                let error = store
                    .persist_in(root.path())
                    .expect_err("ownership must succeed first");
                assert!(error.to_string().contains("vault recovery required"));
                assert_eq!(std::fs::read(&vault_path).unwrap(), damaged);
                assert_eq!(std::fs::read(&metadata_path).unwrap(), original_metadata);

                std::fs::write(&vault_path, original_vault).unwrap();
                let restored = AccountStore::load_in(root.path());
                assert_eq!(restored.accounts[0].config.display_name, "Original");
            }
        }

        phase a_mail_account_from_the_workspaces_accounts_reads_back_without_a_password;
        {
            use accounts::person::PersonAccounts;
            let root = temp_root();
            let config = AccountConfig {
                display_name: "Work".into(),
                email: "me@work.example".into(),
                password: "kept-on-the-device".into(),
                ..AccountConfig::default()
            };
            let account = Account::new(config.clone());
            let mut vault = SecretVault::open_in(&root);
            vault.put(
                &AccountId::from(account.id.clone()),
                &accounts::Secret::password(config.password),
            );
            vault.save();
            let mut replica = ui::tilo_resource::Replica::<PersonAccounts>::new();
            replica
                .apply(PersonAccounts::saving(&account.to_stored()))
                .unwrap();
            let person = replica.state().unwrap();
            let shown = AccountStore::from_shared(&person.store());
            let mut unsigned = account;
            unsigned.config.password.clear();
            assert_eq!(shown.accounts, vec![unsigned]);
            assert!(
                !format!("{person:?}").contains("kept-on-the-device"),
                "the password never enters the resource"
            );
            std::fs::remove_dir_all(&root).ok();
        }
    }

    fn temp_root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tilo-mail-accounts-{}", uuid::Uuid::new_v4()))
    }
}

#[cfg(test)]
mod import_tests;
