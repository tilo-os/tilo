//! Mail's view of the shared multi-account store.
//!
//! ## One shared store, one vault
//! Account *management* in Tilo lives in the **Settings app**, not in Mail —
//! the same way display, network, and Bluetooth settings do. Mail only
//! *consumes* the account list to fetch and send. The canonical store is the
//! shared [`accounts`] crate: a single JSON document under
//! `$XDG_CONFIG_HOME/tilo/accounts/` holding every account of every type, with
//! credentials kept out of it entirely and sealed in the encrypted
//! [`accounts::AccountCredentials`].
//!
//! This module is the mail-side adapter over that store. It keeps the
//! ergonomic in-memory shapes the rest of the app works with — [`Account`]
//! (an [`AccountConfig`] plus per-account sidebar state) and [`AccountStore`]
//! (the list plus the active index) — and hydrates them from the shared store
//! and vault ([`AccountStore::load_in`]). The type-specific connection settings
//! ([`AccountConfig`]) are serialised into each shared account's opaque
//! `config`; the password is read back from the vault, never from the JSON.

use std::path::Path;

use accounts::person::PersonAccounts;
use accounts::{
    Account as StoredAccount, AccountCredentials, AccountId, AccountStore as SharedStore,
    AccountTypeId,
};
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{Id, Invoke};

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
    /// Per-account so a user can keep work expanded and personal collapsed;
    /// kept in the person's Mail preferences, not the shared account store.
    #[serde(skip)]
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
        Self {
            id,
            config,
            collapsed: false,
            tint: 0,
        }
    }

    /// The opaque, secret-free config payload persisted into a shared
    /// [`StoredAccount`]. The password is `#[serde(skip)]` on [`AccountConfig`],
    /// so it never reaches this JSON — it lives only in the vault.
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

    /// Rebuild a mail account from a shared [`StoredAccount`], reading the
    /// password back from `vault`. `None` when the stored config does not
    /// deserialise (a store written by a newer type version).
    fn from_stored(stored: &StoredAccount, vault: &AccountCredentials) -> Option<Self> {
        let payload = stored.config_as::<StoredConfig>()?;
        let mut config = payload.config;
        config.import_pending |= stored.type_id.as_str() == accounts::thunderbird::PENDING_TYPE;
        if !config.import_pending
            && let Some(secret) = vault.get(&stored.id)
        {
            config.password = secret.password;
        }
        Some(Self {
            id: stored.id.to_string(),
            config,
            collapsed: false,
            tint: stored.tint,
        })
    }
}

/// The opaque config payload a mail [`Account`] serialises into a shared
/// [`StoredAccount`]. The `#[serde(skip)]` password on [`AccountConfig`] keeps
/// the credential out of this JSON by construction.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredConfig {
    #[serde(flatten)]
    config: AccountConfig,
}

/// Mail's in-memory account list: the accounts plus which one is shown.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AccountStore {
    pub accounts: Vec<Account>,
    /// Index of the account whose mailbox is currently shown. Clamped on read.
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

    /// Compatibility loader; reports a sanitized error if custody cannot be opened.
    pub fn load() -> Self {
        Self::try_load().unwrap_or_else(|_| {
            tracing::error!("mail account credentials could not be opened");
            Self::default()
        })
    }

    /// Load accounts, returning credential custody failures to the caller.
    ///
    /// # Errors
    /// Returns an error if the config root or protected credential storage is unavailable.
    pub fn try_load() -> Result<Self, String> {
        let root = accounts::config_root().ok_or("no config dir for mail accounts")?;
        let credentials = AccountCredentials::open()?;
        Ok(Self::from_shared(
            &SharedStore::load_in(&root),
            &credentials,
        ))
    }

    /// Compatibility loader under an explicit recovery/test config root.
    pub fn load_in(config_root: &Path) -> Self {
        Self::try_load_in(config_root).unwrap_or_else(|_| {
            tracing::error!("mail account credentials could not be opened");
            Self::default()
        })
    }

    /// Load accounts under an explicit root, returning custody failures.
    ///
    /// # Errors
    /// Returns an error if the existing credential vault cannot be opened safely.
    pub fn try_load_in(config_root: &Path) -> Result<Self, String> {
        let credentials = AccountCredentials::open_in(config_root)?;
        Ok(Self::from_shared(
            &SharedStore::load_in(config_root),
            &credentials,
        ))
    }

    /// Project the shared accounts using the trusted account-owner credentials.
    pub fn from_store(shared: &SharedStore, vault: &AccountCredentials) -> Self {
        Self::from_shared(shared, vault)
    }

    fn from_shared(shared: &SharedStore, vault: &AccountCredentials) -> Self {
        let ty = AccountTypeId::from(TYPE_ID);
        let accounts: Vec<Account> = shared
            .list(&ty)
            .into_iter()
            .chain(shared.list(&accounts::thunderbird::PENDING_TYPE.into()))
            .filter_map(|stored| Account::from_stored(stored, vault))
            .collect();
        // Map the shared store's active-id back to Mail's active-index.
        let active = shared
            .active_id(&ty)
            .and_then(|id| accounts.iter().position(|a| a.id == id.as_str()))
            .unwrap_or(0);
        Self { accounts, active }
    }

    /// Persist to the default Tilo config root
    /// (`$XDG_CONFIG_HOME/tilo/accounts/`). The in-app account add/edit/remove
    /// flow calls this so accounts survive a restart. An unavailable config
    /// root is an error, not a successful save.
    ///
    /// # Errors
    ///
    /// Returns `Err` if writing the shared account store or the secret vault
    /// fails (see [`AccountStore::persist_in`]).
    pub fn persist(&self) -> std::io::Result<()> {
        let root = accounts::config_root()
            .ok_or_else(|| std::io::Error::other("no config dir for mail accounts"))?;
        self.persist_with_vault(
            &root,
            AccountCredentials::open().map_err(std::io::Error::other)?,
        )
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
                vault.put(
                    &id,
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

    /// The edits that make the person's accounts hold these mail accounts:
    /// each new or changed one saved, each mail account the person has that
    /// Mail no longer holds removed, and the shown one active. With them, the
    /// ids removed, whose secrets go too.
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
            let stored = account.to_stored();
            if held.get(&stored.id) != Some(&stored) {
                edits.push(PersonAccounts::saving(&stored));
            }
        }
        if let Some(active) = self.accounts.get(self.active)
            && held.active_id(&ty).map(AccountId::as_str) != Some(active.id.as_str())
        {
            edits.push(PersonAccounts::SET_ACTIVE_ACCOUNT.with(Id::from_raw(active.id.clone())));
        }
        (edits, removed)
    }

    /// Keep this device's secrets of these mail accounts under `config_root`:
    /// each password, an OAuth account's sealed token left as it is, and the
    /// secrets of the `removed` accounts dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the vault cannot be saved.
    pub fn keep_secrets_in(
        &self,
        config_root: &Path,
        removed: &[AccountId],
    ) -> std::io::Result<()> {
        let mut vault = AccountCredentials::open_in(config_root).map_err(std::io::Error::other)?;
        for id in removed {
            vault.remove(id);
        }
        for acct in &self.accounts {
            if acct.config.import_pending {
                continue;
            }
            let id = AccountId::from(acct.id.clone());
            if acct.config.password.is_empty() {
                if acct.config.auth != crate::model::AuthMethod::OAuth {
                    vault.remove(&id);
                }
            } else {
                vault.put(
                    &id,
                    &accounts::Secret::password(acct.config.password.clone()),
                );
            }
        }
        vault.try_save().map_err(std::io::Error::other)
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
        self.active_account()
            .map(|a| a.config.clone())
            .unwrap_or_default()
    }

    pub fn set_active(&mut self, idx: usize) {
        if idx < self.accounts.len() {
            self.active = idx;
        }
    }

    /// Add (or replace, by id) an account and make it active.
    pub fn upsert(&mut self, account: Account) {
        if let Some(idx) = self.accounts.iter().position(|a| a.id == account.id) {
            // `idx` came from `position` on this same vec, so the slot exists.
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

    #[test]
    fn legacy_account_credentials_load_and_damaged_custody_is_reported() {
        let root = temp_root();
        let id = AccountId::from("legacy@example.test");
        let mut shared = SharedStore::new();
        let mut account = Account::new(cfg("legacy@example.test", "Legacy"));
        account.id = id.to_string();
        shared.upsert(account.to_stored());
        shared.save_in(&root).unwrap();
        let mut vault = SecretVault::open_in(&root);
        vault.put(&id, &accounts::Secret::password("legacy-fixture-password"));
        vault.try_save().unwrap();

        let loaded = AccountStore::try_load_in(&root).unwrap();
        assert_eq!(
            loaded.accounts[0].config.password,
            "legacy-fixture-password"
        );
        let metadata = std::fs::read(accounts::store_path_in(&root)).unwrap();
        let vault_path = accounts::vault_dir_in(&root).join("vault.enc");
        std::fs::write(&vault_path, b"damaged").unwrap();
        assert!(AccountStore::try_load_in(&root).is_err());
        assert_eq!(std::fs::read(&vault_path).unwrap(), b"damaged");
        assert_eq!(
            std::fs::read(accounts::store_path_in(&root)).unwrap(),
            metadata
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn temp_root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tilo-mail-accounts-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn account_store_upserts_clamps_and_round_trips_the_shared_store() {
        // Phase `upsert_replaces_by_id_not_appends`.
        {
            let mut store = AccountStore::new();
            store.upsert(Account::new(cfg("a@x.example", "A")));
            let mut updated = Account::new(cfg("a@x.example", "A renamed"));
            updated.id = "a@x.example".into();
            store.upsert(updated);
            assert_eq!(store.len(), 1);
            assert_eq!(store.accounts[0].config.display_name, "A renamed");
        }

        // Phase `active_is_clamped_after_remove`.
        {
            let mut store = AccountStore::new();
            store.upsert(Account::new(cfg("a@x.example", "A")));
            store.upsert(Account::new(cfg("b@y.example", "B")));
            store.set_active(1);
            store.remove("b@y.example");
            // active was 1, now only index 0 exists — must clamp, not panic.
            assert_eq!(store.active_account().unwrap().config.email, "a@x.example");
        }

        // Phase `shared_store_round_trips_accounts_and_active`.
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

        // Phase `removing_an_account_reconciles_the_shared_store_on_persist`.
        {
            let root = temp_root();
            // Two accounts persisted.
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

    #[test]
    fn account_secrets_live_in_the_vault_and_a_damaged_vault_blocks_changes() {
        // Phase `removed_account_secret_is_purged_from_the_vault`.
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

        // Phase `persist_preserves_a_sealed_oauth_token_for_a_passwordless_account`.
        {
            let root = temp_root();
            // Sign-in sealed an OAuth token before the account is saved.
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
            // Persist an OAuth (passwordless) account with that id.
            let mut c = cfg("g@gmail.com", "G");
            c.auth = crate::model::AuthMethod::OAuth;
            c.password.clear();
            let store = AccountStore::single(c);
            store.persist_in(&root).expect("persist");
            // The OAuth token must NOT be clobbered by the password-secret path.
            let secret = SecretVault::open_in(&root)
                .get(&id)
                .expect("token survives");
            assert_eq!(secret.token.as_deref(), Some("{\"access_token\":\"tok\"}"));
            std::fs::remove_dir_all(&root).ok();
        }

        // Phase `password_persists_to_the_vault_not_the_store_json`.
        {
            let root = temp_root();
            let mut c = cfg("a@x.example", "A");
            c.password = "hunter2".into();
            let store = AccountStore::single(c);
            store.persist_in(&root).expect("persist");

            // The plaintext store must never carry the password …
            let store_json =
                std::fs::read_to_string(accounts::store_path_in(&root)).expect("store written");
            assert!(
                !store_json.contains("hunter2"),
                "password must not reach the shared store JSON"
            );

            // … but a reload recovers it from the vault.
            let back = AccountStore::load_in(&root);
            assert_eq!(
                back.accounts[0].config.password, "hunter2",
                "password rehydrates from the vault"
            );
            std::fs::remove_dir_all(&root).ok();
        }

        // Phase `damaged_vault_rejects_account_changes_and_preserves_recovery_data`.
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
                .expect_err("custody must succeed first");
            assert!(error.to_string().contains("vault recovery required"));
            assert_eq!(std::fs::read(&vault_path).unwrap(), damaged);
            assert_eq!(std::fs::read(&metadata_path).unwrap(), original_metadata);

            std::fs::write(&vault_path, original_vault).unwrap();
            let restored = AccountStore::load_in(root.path());
            assert_eq!(restored.accounts[0].config.display_name, "Original");
            assert_eq!(
                restored.accounts[0].config.password,
                "fixture-original-secret"
            );
        }
    }

    /// A mail account Settings saved in the person's accounts record reads
    /// back in Mail as it was, its password from this device's vault and
    /// never from the record.
    #[test]
    fn a_mail_account_from_the_persons_record_reads_back_with_the_devices_password() {
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
        let shown = AccountStore::from_store(
            &person.store(),
            &AccountCredentials::open_in(&root).unwrap(),
        );
        assert_eq!(shown.accounts, vec![account]);
        assert!(
            !format!("{person:?}").contains("kept-on-the-device"),
            "the password never enters the record"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Mail's own setup sheet keeps its accounts through the person's
    /// record: adding saves the account and makes it active, removing it
    /// removes it from the record and drops its secret.
    #[test]
    fn the_setup_sheets_edits_become_the_persons_account_edits() {
        use accounts::person::PersonAccounts;
        let root = temp_root();
        let mut replica = ui::tilo_resource::Replica::<PersonAccounts>::new();
        let mut store = AccountStore::single(AccountConfig {
            display_name: "Work".into(),
            email: "me@work.example".into(),
            password: "secret-1".into(),
            ..AccountConfig::default()
        });
        let (edits, removed) = store.edits_for(&replica.state().unwrap());
        assert!(removed.is_empty());
        for edit in edits {
            replica.apply(edit).unwrap();
        }
        store.keep_secrets_in(&root, &removed).unwrap();
        let person = replica.state().unwrap().store();
        assert_eq!(
            person
                .active(&AccountTypeId::from(TYPE_ID))
                .map(|a| a.id.as_str()),
            Some("me@work.example")
        );
        assert_eq!(
            SecretVault::open_in(&root)
                .get(&AccountId::from("me@work.example"))
                .map(|secret| secret.password),
            Some("secret-1".to_owned())
        );
        let (again, _) = store.edits_for(&replica.state().unwrap());
        assert!(again.is_empty(), "nothing changed, nothing to edit");

        store.accounts.clear();
        let (edits, removed) = store.edits_for(&replica.state().unwrap());
        assert_eq!(removed, vec![AccountId::from("me@work.example")]);
        for edit in edits {
            replica.apply(edit).unwrap();
        }
        store.keep_secrets_in(&root, &removed).unwrap();
        assert!(replica.state().unwrap().accounts.is_empty());
        assert!(
            SecretVault::open_in(&root)
                .get(&AccountId::from("me@work.example"))
                .is_none()
        );
        std::fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod import_tests;
