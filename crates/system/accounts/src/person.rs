//! The person's accounts as a record of their own: the accounts they
//! configured follow them to every device they sign in on, never to a
//! collaborator.
//!
//! [`PersonAccounts`] is a resource kept in the person's own room, a
//! [`SharedRecord`]: its owner app ([`ACCOUNTS_OWNER`], Settings) and Mail,
//! whose provider sign-in adds mail accounts, edit it through its actions;
//! every other app reads it. Each app keeps its copy of the one record. It holds what an account *is* — its type, its name,
//! its connection settings — and never a credential: secrets stay in this
//! device's [`SecretVault`](crate::SecretVault) under the account's id, so a
//! device that has not been signed in yet shows the account and asks for its
//! secret.
//!
//! Services that run outside any app (the storage daemon, the credential
//! custodian, the file manager's remote places) read this device's
//! [`AccountStore`] file. The editors keep that file a projection of the
//! record ([`PersonAccounts::project_in`]); it is never edited directly. A
//! file that is not a projection yet holds accounts configured on this
//! device before the record kept them: the first editor to open takes them
//! in once ([`PersonAccounts::taking_in`]).

use std::collections::BTreeMap;
use std::path::Path;

use tilo_resource::{Collection, Id, Invoke, Json, Model, Refusal, SharedRecord, resource};

use crate::{Account, AccountId, AccountStore, AccountTypeId};

/// The app that keeps the person's accounts.
pub const ACCOUNTS_OWNER: &str = "so.tilo.settings";

/// The other apps that change the person's accounts: Mail adds the mail
/// accounts its provider sign-in sets up.
pub const ACCOUNTS_EDITORS: &[&str] = &["so.tilo.mail"];

/// One account of the person's, without its secret.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct PersonAccount {
    /// The kind of account (`"email"`, `"caldav"`, …).
    pub type_id: String,
    /// The name shown for it ("Work — Fastmail").
    pub display_name: String,
    /// Type-specific connection settings. Never a secret.
    pub config: Json<serde_json::Value>,
    /// Whether the person uses it now.
    pub enabled: bool,
    /// Tints the account's avatar so several read as distinct.
    pub tint: u8,
}

/// Every account the person configured, and the one each type uses.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct PersonAccounts {
    /// Keyed by the account's id, the id its secret is kept under.
    pub accounts: Collection<PersonAccount>,
    /// The active account id per type; a type without one uses its first.
    pub active: BTreeMap<String, String>,
}

#[resource(kind = "tilo.accounts", label = "Accounts")]
impl PersonAccounts {
    /// Save an account: add it, or replace the one with its id.
    #[action]
    pub fn save_account(
        &mut self,
        #[param("The account's id, which its secret is kept under")] id: String,
        #[param("The kind of account, such as email or caldav")] type_id: String,
        #[param("The name shown for it")] display_name: String,
        #[param("Its connection settings, without any secret")] config: serde_json::Value,
        #[param("Whether it is in use")] enabled: bool,
        #[param("Avatar tint")] tint: u8,
    ) -> Result<(), Refusal> {
        if id.trim().is_empty() || type_id.trim().is_empty() {
            return Err(Refusal::from("an account needs an id and a type"));
        }
        let key = Id::from_raw(id.clone());
        let account = PersonAccount {
            type_id: type_id.clone(),
            display_name,
            config: Json(config),
            enabled,
            tint,
        };
        if let Some(existing) = self.accounts.get_mut(&key) {
            *existing = account;
        } else {
            self.accounts.push(key, account);
        }
        self.active.entry(type_id).or_insert(id);
        Ok(())
    }

    /// Remove account.
    #[action]
    pub fn remove_account(&mut self, account: Id) -> Result<(), Refusal> {
        let removed = self
            .accounts
            .remove(&account)
            .ok_or_else(|| Refusal::from("there is no such account"))?;
        if self.active.get(&removed.type_id).map(String::as_str) == Some(account.as_str()) {
            match self
                .accounts
                .iter()
                .find(|(_, other)| other.type_id == removed.type_id)
            {
                Some((next, _)) => {
                    self.active
                        .insert(removed.type_id.clone(), next.as_str().to_owned());
                }
                None => {
                    self.active.remove(&removed.type_id);
                }
            }
        }
        Ok(())
    }

    /// Use an account for its type.
    #[action]
    pub fn set_active_account(&mut self, account: Id) -> Result<(), Refusal> {
        let type_id = self
            .accounts
            .get(&account)
            .ok_or_else(|| Refusal::from("there is no such account"))?
            .type_id
            .clone();
        self.active.insert(type_id, account.as_str().to_owned());
        Ok(())
    }
}

impl SharedRecord for PersonAccounts {
    const OWNER: &'static str = ACCOUNTS_OWNER;
    const EDITORS: &'static [&'static str] = ACCOUNTS_EDITORS;
}

impl PersonAccounts {
    /// The edits that take in the accounts of `device`, a store this device
    /// kept before the record did: each account the record lacks, and the
    /// active one of a type the record has none of. Once the file is the
    /// record's projection it brings nothing back — an account removed on
    /// another device stays removed — except into a copy of the record that
    /// holds no account yet, which takes what this device shows.
    #[must_use]
    pub fn taking_in(&self, device: &AccountStore) -> Vec<Invoke<Self>> {
        if device.is_projection() && !self.accounts.is_empty() {
            return Vec::new();
        }
        let mut edits = Vec::new();
        for account in device.all() {
            if !self.accounts.contains(&Id::from_raw(account.id.0.clone())) {
                edits.push(Self::saving(account));
            }
        }
        for account in device.all() {
            let type_id = account.type_id.as_str();
            if !self.active.contains_key(type_id)
                && device.active_id(&account.type_id) == Some(&account.id)
            {
                edits.push(Self::SET_ACTIVE_ACCOUNT.with(Id::from_raw(account.id.0.clone())));
            }
        }
        edits
    }

    /// The accounts as the device's [`AccountStore`] holds them: what
    /// readers that work with the store's types take.
    #[must_use]
    pub fn store(&self) -> AccountStore {
        let mut store = AccountStore::new();
        for (id, account) in self.accounts.iter() {
            store.upsert(Account {
                id: AccountId::new(id.as_str()),
                type_id: AccountTypeId::new(account.type_id.clone()),
                display_name: account.display_name.clone(),
                config: account.config.0.clone(),
                enabled: account.enabled,
                tint: account.tint,
                owner: None,
            });
        }
        for (type_id, id) in &self.active {
            store.set_active(
                &AccountTypeId::new(type_id.clone()),
                &AccountId::new(id.clone()),
            );
        }
        store.as_projection()
    }

    /// The edit that saves `account` as it is.
    #[must_use]
    pub fn saving(account: &Account) -> Invoke<Self> {
        // An account without connection settings is saved with none: the
        // door refuses a missing value.
        let config = if account.config.is_null() {
            serde_json::json!({})
        } else {
            account.config.clone()
        };
        Self::SAVE_ACCOUNT.with((
            account.id.0.clone(),
            account.type_id.0.clone(),
            account.display_name.clone(),
            config,
            account.enabled,
            account.tint,
        ))
    }

    /// Keep this device's account file under `config_root` the projection
    /// of the person's accounts, for the services that read it. Writes only
    /// when it differs. Whether it wrote.
    ///
    /// # Errors
    /// The file could not be written.
    pub fn project_in(&self, config_root: &Path) -> std::io::Result<bool> {
        let store = self.store();
        if AccountStore::load_in(config_root) == store {
            return Ok(false);
        }
        store.save_in(config_root)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Replica;

    fn save(replica: &mut Replica<PersonAccounts>, id: &str, type_id: &str, host: &str) {
        replica
            .apply(PersonAccounts::SAVE_ACCOUNT.with((
                id.into(),
                type_id.into(),
                format!("{id} inbox"),
                serde_json::json!({ "host": host }),
                true,
                2,
            )))
            .unwrap();
    }

    #[test]
    fn accounts_are_saved_by_id_and_each_type_keeps_an_active_one() {
        let mut replica = Replica::<PersonAccounts>::new();
        save(&mut replica, "a@x.example", "email", "imap.x");
        save(&mut replica, "b@y.example", "email", "imap.y");
        save(&mut replica, "work", "caldav", "dav.example");
        save(&mut replica, "a@x.example", "email", "imap.new");

        let store = replica.state().unwrap().store();
        assert_eq!(
            store.list(&"email".into()).len(),
            2,
            "saving again replaces"
        );
        assert_eq!(
            store.get(&"a@x.example".into()).unwrap().config["host"],
            "imap.new"
        );
        assert_eq!(
            store.active(&"email".into()).unwrap().id,
            "a@x.example".into()
        );

        replica
            .apply(PersonAccounts::SET_ACTIVE_ACCOUNT.with(Id::from_raw("b@y.example")))
            .unwrap();
        assert_eq!(
            replica
                .state()
                .unwrap()
                .store()
                .active(&"email".into())
                .unwrap()
                .id,
            "b@y.example".into()
        );
        replica
            .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("b@y.example")))
            .unwrap();
        let store = replica.state().unwrap().store();
        assert_eq!(
            store.active(&"email".into()).unwrap().id,
            "a@x.example".into(),
            "removing the active account falls back to another"
        );
        assert!(
            replica
                .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("gone")))
                .is_err()
        );
    }

    #[test]
    fn the_device_file_is_a_projection_without_secrets() {
        let mut replica = Replica::<PersonAccounts>::new();
        save(&mut replica, "a@x.example", "email", "imap.x");
        let root =
            std::env::temp_dir().join(format!("tilo-person-accounts-{}", uuid::Uuid::new_v4()));
        let accounts = replica.state().unwrap();
        assert!(accounts.project_in(&root).unwrap());
        assert!(
            !accounts.project_in(&root).unwrap(),
            "unchanged, not rewritten"
        );
        assert_eq!(AccountStore::load_in(&root), accounts.store());
        let raw = std::fs::read_to_string(crate::store_path_in(&root)).unwrap();
        assert!(!raw.to_lowercase().contains("password"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn saving_a_store_account_round_trips() {
        let mut account = Account::new("cal-1", "caldav");
        account.display_name = "Work".into();
        account.config = serde_json::json!({ "url": "https://dav.example" });
        let mut replica = Replica::<PersonAccounts>::new();
        replica.apply(PersonAccounts::saving(&account)).unwrap();
        assert_eq!(
            replica.state().unwrap().store().get(&"cal-1".into()),
            Some(&account)
        );
    }

    #[test]
    fn a_device_file_is_taken_in_once_then_follows_the_record() {
        let mut replica = Replica::<PersonAccounts>::new();
        save(&mut replica, "roamed", "caldav", "dav.example");
        let mut device = AccountStore::new();
        device.upsert(Account::new("local@x.example", "email"));
        device.upsert(Account::new("roamed", "caldav"));
        for edit in replica.state().unwrap().taking_in(&device) {
            replica.apply(edit).unwrap();
        }
        let person = replica.state().unwrap().store();
        assert!(person.get(&"local@x.example".into()).is_some(), "taken in");
        assert_eq!(
            person.active(&"email".into()).unwrap().id,
            "local@x.example".into()
        );
        assert_eq!(
            person.get(&"roamed".into()).unwrap().config["host"],
            "dav.example",
            "the record's own account is kept as it is"
        );

        // Removed on another device: the projected file never brings it back.
        replica
            .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("local@x.example")))
            .unwrap();
        let projected = person;
        assert!(projected.is_projection());
        assert!(replica.state().unwrap().taking_in(&projected).is_empty());
    }
}
