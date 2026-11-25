//! Mail's shape of the workspace's accounts.
//!
//! Account management lives in Settings, not in Mail, which only consumes the account list to
//! fetch and send. The accounts are the workspace's one record ([`AccountList`], kind
//! `tilo.accounts`), which every member and device of the workspace reads as its policy allows;
//! no secret is in it.
//!
//! This module keeps the shape the rest of Mail works with, [`Account`], hydrates it from the
//! record ([`accounts_of`]) and says what edits keep a list of them in it ([`edits_for`]). The
//! connection settings are serialised into each account's opaque `config`. A password is never
//! read here: the device releases it from the workspace's vault to the app that signs in,
//! by the entry the account names.

use accounts::person::AccountList;
use accounts::{Account as StoredAccount, AccountId};
use serde::{Deserialize, Serialize};
use tilo_resource::{Id, Invoke};

use crate::model::AccountConfig;

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

    /// The account as the workspace's record keeps it: its connection settings, and never its
    /// password.
    #[must_use]
    pub fn to_stored(&self) -> StoredAccount {
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

    /// The account `stored` describes, without a password.
    #[must_use]
    pub fn from_stored(stored: &StoredAccount) -> Option<Self> {
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

/// Every account of the workspace's record that Mail works with: the email accounts and the
/// ones an import from another mail program is still bringing in, in the record's order, each
/// without a password.
#[must_use]
pub fn accounts_of(record: &AccountList) -> Vec<Account> {
    accounts_from(&record.all())
}

/// [`accounts_of`], for accounts already read out of the record: what a connector process is
/// sent, which holds no record of its own.
#[must_use]
pub fn accounts_from(stored: &[StoredAccount]) -> Vec<Account> {
    stored
        .iter()
        .filter(|stored| {
            stored.type_id.as_str() == TYPE_ID
                || stored.type_id.as_str() == accounts::thunderbird::PENDING_TYPE
        })
        .filter_map(Account::from_stored)
        .collect()
}

/// The edits that make the workspace's record hold `accounts` as Mail does now: each saved as
/// it is, and every email account the record keeps that `accounts` does not named for removal.
#[must_use]
pub fn edits_for(
    accounts: &[Account],
    record: &AccountList,
) -> (Vec<Invoke<AccountList>>, Vec<AccountId>) {
    let mut edits = Vec::new();
    let mut removed = Vec::new();
    for stale in record.of_type(TYPE_ID) {
        if !accounts.iter().any(|a| a.id == stale.id.as_str()) {
            edits.push(AccountList::REMOVE_ACCOUNT.with(Id::from_raw(stale.id.0.clone())));
            removed.push(stale.id.clone());
        }
    }
    for account in accounts {
        let mut stored = account.to_stored();
        let kept = record.account(&account.id);
        // The vault entry its password is in is the workspace's to name, which Mail's settings
        // of the account do not carry: it stays as the record names it.
        if let Some(item) = kept
            .as_ref()
            .and_then(|kept| accounts::email::password_item(&kept.config))
        {
            accounts::email::set_password_item(&mut stored.config, Some(item));
        }
        if kept.as_ref() != Some(&stored) {
            edits.push(AccountList::saving(&stored));
        }
    }
    (edits, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mail_account_from_the_workspaces_accounts_reads_back_without_a_password() {
        use accounts::person::AccountList;
        let config = AccountConfig {
            display_name: "Work".into(),
            email: "me@work.example".into(),
            password: "typed-but-never-kept".into(),
            ..AccountConfig::default()
        };
        let account = Account::new(config);
        let mut replica =
            tilo_resource::Replica::<AccountList>::new().in_app("so.tilo.model", "Model");
        replica
            .apply(AccountList::saving(&account.to_stored()))
            .unwrap();
        let person = replica.state().unwrap();
        let shown = accounts_of(&person);
        let mut unsigned = account;
        unsigned.config.password.clear();
        assert_eq!(shown, vec![unsigned]);
        assert!(
            !format!("{person:?}").contains("typed-but-never-kept"),
            "the password never enters the resource"
        );
    }
}

#[cfg(test)]
mod import_tests;
