//! Mail's view of the shared multi-account store.
//!
//! Account management lives in Settings, not in Mail, which only consumes the account list to
//! fetch and send. The accounts are the workspace's accounts record ([`PersonAccounts`]), which
//! holds every account of every type and no secret.
//!
//! This module keeps the shapes the rest of Mail works with, [`Account`] and
//! [`AccountStore`], reads them from that record ([`AccountStore::from_shared`]) and makes the
//! edits that keep them there ([`AccountStore::edits_for`]). The connection settings are
//! serialised into each account's opaque `config`. A password is never read here: the session
//! releases it from the workspace's vault to whoever signs in (`accounts::sign_ins`).

use std::path::Path;

use accounts::person::PersonAccounts;
use accounts::{Account as StoredAccount, AccountId, AccountStore as SharedStore, AccountTypeId};
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
    use ui::tilo_resource::Replica;

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

    /// Keep `store`'s accounts in `record` as Mail's edits do, and read them back as Mail does.
    fn kept(record: &mut Replica<PersonAccounts>, store: &AccountStore) -> AccountStore {
        let (edits, _) = store.edits_for(&record.state().unwrap());
        for edit in edits {
            record.apply(edit).unwrap();
        }
        AccountStore::from_shared(&record.state().unwrap().store())
    }

    tilo_test_env::scenario! {
        /// Mail's accounts, kept in the workspace's accounts record and read back from it.
        fn the_account_store_keeps_accounts_in_the_workspaces_record;

        phase account_store_upserts_clamps_and_round_trips_the_record;
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
                let mut record = Replica::<PersonAccounts>::new();
                let mut store = AccountStore::new();
                store.upsert(Account::new(cfg("a@x.example", "A")));
                store.upsert(Account::new(cfg("b@y.example", "B")));
                let back = kept(&mut record, &store);
                assert_eq!(back.len(), 2);
                assert_eq!(back.accounts[1].config.display_name, "B");

                // A removed account leaves the record, so a read does not bring it back.
                store.remove("a@x.example");
                let back = kept(&mut record, &store);
                assert_eq!(back.len(), 1);
                assert_eq!(back.accounts[0].config.email, "b@y.example");
            }
        }

        phase a_mail_account_from_the_workspaces_accounts_reads_back_without_a_password;
        {
            let config = AccountConfig {
                display_name: "Work".into(),
                email: "me@work.example".into(),
                password: "typed-into-settings".into(),
                ..AccountConfig::default()
            };
            let account = Account::new(config);
            let mut record = Replica::<PersonAccounts>::new();
            let shown = kept(&mut record, &AccountStore {
                accounts: vec![account.clone()],
                active: 0,
            });
            let mut unsigned = account;
            unsigned.config.password.clear();
            assert_eq!(shown.accounts, vec![unsigned]);
            assert!(
                !format!("{:?}", record.state().unwrap()).contains("typed-into-settings"),
                "the password never enters the resource"
            );
        }
    }
}

#[cfg(test)]
mod import_tests;
