//! Keeping an account in the shared store and its secret in the vault, and
//! reading them back: what a device with a window does.

use std::path::Path;

use accounts::{
    Account as StoredAccount, AccountCredentials, AccountId, AccountStore as SharedStore,
    AccountTypeId,
};
use serde::{Deserialize, Serialize};

use super::{AccountStore, CalDavConfig, CalendarAccount, RemoteCalendar, TYPE_ID};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredConfig {
    #[serde(flatten)]
    config: CalDavConfig,
    #[serde(default)]
    calendars: Vec<RemoteCalendar>,
    #[serde(default)]
    collapsed: bool,
    /// Whether the account is connected for reading only, beside its server
    /// and its user name. An account that is written to says nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    read_only: bool,
}

impl CalendarAccount {
    /// The opaque, secret-free config payload persisted into a shared
    /// [`StoredAccount`]. The `#[serde(skip)]` password keeps the credential
    /// out of this JSON; it lives only in the vault.
    fn to_stored(&self) -> StoredAccount {
        let mut stored = StoredAccount::new(AccountId::from(self.id.clone()), TYPE_ID);
        stored.display_name = self.label().to_string();
        stored.set_config(&StoredConfig {
            config: self.config.clone(),
            calendars: self.kept_calendars(),
            collapsed: self.collapsed,
            read_only: self.read_only,
        });
        stored
    }

    /// The payload a test keeps, so what it saves reads back as an account.
    pub fn to_stored_for_test(&self) -> StoredAccount {
        self.to_stored()
    }

    fn from_stored(stored: &StoredAccount, vault: Option<&AccountCredentials>) -> Option<Self> {
        let payload: StoredConfig = stored
            .config
            .get("calendar")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .or_else(|| stored.config_as::<StoredConfig>())?;
        let mut config = payload.config;
        if config.display_name.is_empty() {
            config.display_name.clone_from(&stored.display_name);
        }
        if let Some(secret) = vault.and_then(|vault| vault.get_for(stored)) {
            #[cfg(feature = "live")]
            {
                config.password = tilo_dav::oauth::calendar_password(&secret);
            }
            #[cfg(not(feature = "live"))]
            {
                if secret.token.is_none() {
                    config.password = secret.password;
                }
            }
        }
        if stored
            .config
            .get("calendar_provider")
            .and_then(serde_json::Value::as_str)
            == Some("google")
        {
            config.oauth_account_id = Some(stored.id.to_string());
            config.password.clear();
        }
        let mut account = Self {
            id: stored.id.to_string(),
            config,
            calendars: payload.calendars,
            collapsed: payload.collapsed,
            read_only: payload.read_only,
        };
        account.calendars = account.kept_calendars();
        Some(account)
    }
}

impl AccountStore {
    pub fn load_from_settings_in(config_root: &Path) -> Self {
        Self::try_load_in(config_root).unwrap_or_else(|_| {
            tracing::error!("calendar account credentials could not be opened");
            Self::default()
        })
    }

    pub fn try_load_in(config_root: &Path) -> Result<Self, String> {
        let credentials = AccountCredentials::open_in(config_root)?;
        Ok(Self::from_shared(
            &SharedStore::load_in(config_root),
            Some(&credentials),
        ))
    }

    pub fn from_store(shared: &SharedStore, vault: &AccountCredentials) -> Self {
        Self::from_shared(shared, Some(vault))
    }

    pub fn from_shared(shared: &SharedStore, vault: Option<&AccountCredentials>) -> Self {
        let ty = AccountTypeId::from(TYPE_ID);
        let accounts = shared
            .list(&ty)
            .into_iter()
            .chain(shared.all().iter().filter(|stored| {
                stored
                    .config
                    .get("use_calendar")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                    && stored.type_id.as_str() != TYPE_ID
            }))
            .filter(|stored| stored.enabled)
            .filter_map(|stored| CalendarAccount::from_stored(stored, vault))
            .collect();
        Self { accounts }
    }

    /// Write these accounts into the shared store and vault under `config_root`, preserving unrelated
    /// accounts. For a Nextcloud projection only calendar preferences change; its type, services and
    /// sign-in stay.
    ///
    /// # Errors
    ///
    /// When the shared account store cannot be saved to `config_root`.
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
        let mut shared = SharedStore::load_in(config_root);
        let mut vault_changed = false;
        for acct in &self.accounts {
            let id = AccountId::from(acct.id.clone());
            if let Some(source) = shared.get(&id).filter(|source| {
                source.type_id.as_str() != TYPE_ID || acct.config.oauth_account_id.is_some()
            }) {
                let mut stored = source.clone();
                let config = stored
                    .config
                    .as_object_mut()
                    .ok_or_else(|| std::io::Error::other("Nextcloud settings are not an object"))?;
                if let Some(calendar) = config.get_mut("calendar") {
                    calendar["calendars"] = serde_json::to_value(acct.kept_calendars())?;
                    calendar["collapsed"] = acct.collapsed.into();
                } else {
                    config.insert(
                        "calendars".into(),
                        serde_json::to_value(acct.kept_calendars())?,
                    );
                    config.insert("collapsed".into(), acct.collapsed.into());
                }
                shared.upsert(stored);
                continue;
            }
            shared.upsert(acct.to_stored());
            vault_changed = true;
            if acct.config.password.is_empty() {
                vault.remove(&id);
            } else {
                vault.put_for(
                    &acct.to_stored(),
                    &accounts::Secret::password(acct.config.password.clone()),
                );
            }
        }
        if vault_changed {
            vault.try_save().map_err(std::io::Error::other)?;
        }
        shared.save_in(config_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accounts::SecretVault;

    tilo_test_env::scenario! {
        /// The account store: what it writes, what it refuses to publish and what it reads back.
        fn account_credentials_stay_in_the_vault_and_the_store_round_trips;

        phase account_credentials_are_never_serialized_or_debugged;
        {
            let config = CalDavConfig {
                password: "private-calendar-password".into(),
                ..Default::default()
            };
            assert!(
                !serde_json::to_string(&config)
                    .unwrap()
                    .contains("private-calendar-password")
            );
            assert!(!format!("{config:?}").contains("private-calendar-password"));
        }

        phase damaged_credentials_prevent_publishing_account_changes;
        {
            let root = temp_root();
            let store = AccountStore::sample();
            store.persist_in(&root).unwrap();
            let before = std::fs::read(accounts::store_path_in(&root)).unwrap();
            std::fs::write(accounts::vault_dir_in(&root).join("vault.enc"), b"damaged").unwrap();
            let mut changed = store;
            changed.accounts[0].config.display_name = "Must not publish".into();
            assert!(changed.persist_in(&root).is_err());
            assert_eq!(
                std::fs::read(accounts::store_path_in(&root)).unwrap(),
                before
            );
            std::fs::remove_dir_all(root).unwrap();
        }

        phase legacy_account_credentials_load_and_damaged_ownership_is_reported;
        {
            let root = temp_root();
            let id = AccountId::from("legacy@example.test");
            let mut shared = SharedStore::new();
            let mut account = CalendarAccount::new(cfg("Legacy", "legacy@example.test"));
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

        phase account_store_round_trips_accounts_visibility_and_vaulted_secrets;
        {
            {
                let root = temp_root();
                let mut store = AccountStore::new();
                let mut a = CalendarAccount::new(cfg("Work", "a@x.example"));
                a.calendars.push(RemoteCalendar {
                    id: "work/team".into(),
                    name: "Team".into(),
                    color: 1,
                    visible: true,
                    read_only: false,
                });
                a.collapsed = true;
                store.upsert(a);
                store.upsert(CalendarAccount::new(cfg("Home", "b@y.example")));
                store.persist_in(&root).expect("persist");

                let back = AccountStore::load_from_settings_in(&root);
                assert_eq!(back.len(), 2);
                assert_eq!(back.accounts[0].config.username, "a@x.example");
                assert_eq!(back.accounts[0].calendars.len(), 1);
                assert_eq!(back.accounts[0].calendars[0].name, "Team");
                assert!(back.accounts[0].collapsed);
                std::fs::remove_dir_all(&root).ok();
            }
            {
                let root = temp_root();
                let other_id = AccountId::from("unrelated-service");
                let mut shared = SharedStore::new();
                shared.upsert(StoredAccount::new(
                    other_id.clone(),
                    AccountTypeId::from("unrelated"),
                ));
                shared.save_in(&root).expect("seed unrelated account");
                let mut vault = SecretVault::open_in(&root);
                vault.put(&other_id, &accounts::Secret::password("unrelated-secret"));
                vault.save();

                let mut store = AccountStore::sample();
                store.persist_in(&root).expect("persist calendar accounts");
                let calendar = &mut store.accounts[0].calendars[0];
                let id = calendar.id.clone();
                calendar.visible = false;
                store.persist_in(&root).expect("persist visibility toggle");

                let restarted = AccountStore::load_from_settings_in(&root);
                assert!(
                    restarted
                        .all_calendars()
                        .find(|(_, calendar)| calendar.id == id)
                        .is_some_and(|(_, calendar)| !calendar.visible)
                );
                assert!(SharedStore::load_in(&root).get(&other_id).is_some());
                assert!(SecretVault::open_in(&root).get(&other_id).is_some());
                std::fs::remove_dir_all(&root).ok();
            }
            {
                let mut store = AccountStore::new();
                store.upsert(CalendarAccount::new(cfg("A", "a@x.example")));
                let mut updated = CalendarAccount::new(cfg("A renamed", "a@x.example"));
                updated.id = "a@x.example".into();
                store.upsert(updated);
                assert_eq!(store.len(), 1);
                assert_eq!(store.accounts[0].config.display_name, "A renamed");
            }
            {
                let root = temp_root();
                let mut c = cfg("A", "a@x.example");
                c.password = "hunter2".into();
                let mut store = AccountStore::new();
                store.upsert(CalendarAccount::new(c));
                store.persist_in(&root).expect("persist");

                let store_json =
                    std::fs::read_to_string(accounts::store_path_in(&root)).expect("store written");
                assert!(
                    !store_json.contains("hunter2"),
                    "password must not reach the shared store JSON"
                );

                let back = AccountStore::load_from_settings_in(&root);
                assert_eq!(
                    back.accounts[0].config.password, "hunter2",
                    "password rehydrates from the vault"
                );
                std::fs::remove_dir_all(&root).ok();
            }
            {
                let store = AccountStore::sample();
                assert_eq!(store.len(), 2);
                let total = store.all_calendars().count();
                assert_eq!(total, 4);
                let visible = store.all_calendars().filter(|(_, c)| c.visible).count();
                assert_eq!(visible, 3);
            }
        }

        phase an_account_connected_for_reading_only_is_kept_so_with_every_calendar_of_it;
        {
            let root = temp_root();
            let team = |read_only| RemoteCalendar {
                id: "work/team".into(),
                name: "Team".into(),
                color: 1,
                visible: true,
                read_only,
            };
            let mut reading = CalendarAccount::new(cfg("Work", "a@x.example"));
            reading.read_only = true;
            // Its server would let the person write this calendar.
            reading.calendars.push(team(false));
            let mut writing = CalendarAccount::new(cfg("Home", "b@y.example"));
            writing.calendars.push(team(false));
            let mut store = AccountStore::new();
            store.upsert(reading);
            store.upsert(writing);
            store.persist_in(&root).expect("persist");

            // The account's settings say it, beside its server and its user name, and so
            // does every calendar of it. An account that is written to says nothing new.
            let reading_id = AccountId::from("a@x.example");
            let shared = SharedStore::load_in(&root);
            let kept = shared.get(&reading_id).expect("kept").config.clone();
            assert_eq!(kept["read_only"], true, "{kept}");
            assert_eq!(kept["calendars"][0]["read_only"], true, "{kept}");
            let other = shared
                .get(&AccountId::from("b@y.example"))
                .expect("kept")
                .config
                .clone();
            assert!(other.get("read_only").is_none(), "{other}");
            assert_eq!(other["calendars"][0]["read_only"], false, "{other}");

            // Read back, changed as a look at its server's calendars changes an account,
            // and saved again: the account's word stays, and no calendar of it is lifted.
            let mut back = AccountStore::load_from_settings_in(&root);
            assert!(back.accounts[0].read_only && back.accounts[0].calendars[0].read_only);
            assert!(!back.accounts[1].read_only && !back.accounts[1].calendars[0].read_only);
            back.accounts[0].calendars = vec![team(false)];
            back.accounts[0].collapsed = true;
            back.persist_in(&root).expect("persist again");
            let again = AccountStore::load_from_settings_in(&root);
            assert!(again.accounts[0].read_only && again.accounts[0].collapsed);
            assert!(again.accounts[0].calendars[0].read_only);
            assert!(!again.accounts[1].read_only && !again.accounts[1].calendars[0].read_only);

            // Settings in which a calendar lost its own mark and the account kept its word
            // read as an account none of whose calendars can be written.
            let mut shared = SharedStore::load_in(&root);
            let mut stored = shared.get(&reading_id).expect("kept").clone();
            stored.config["calendars"][0]["read_only"] = false.into();
            shared.upsert(stored);
            shared.save_in(&root).expect("save");
            let read = AccountStore::load_from_settings_in(&root);
            assert!(read.accounts[0].read_only && read.accounts[0].calendars[0].read_only);
            std::fs::remove_dir_all(&root).ok();
        }
    }

    /// A password is typed for the server its account names then. However
    /// the account comes to name another server, the password is joined to
    /// it no more: the next sync would send it there.
    #[test]
    fn a_password_typed_for_one_server_is_not_joined_to_an_account_that_names_another() {
        let root = temp_root();
        let mut typed = cfg("Work", "me@example.test");
        typed.server_url = "https://caldav.example/dav/me/".into();
        typed.password = "typed-for-caldav-example".into();
        let mut store = AccountStore::new();
        store.upsert(CalendarAccount::new(typed));
        store.persist_in(&root).unwrap();
        let joined = |address: &str| {
            let mut shared = SharedStore::load_in(&root);
            let mut account = shared.all()[0].clone();
            account.config["server_url"] = address.into();
            shared.upsert(account);
            let vault = AccountCredentials::open_in(&root).unwrap();
            AccountStore::from_store(&shared, &vault).accounts[0]
                .config
                .password
                .clone()
        };
        assert_eq!(
            joined("https://caldav.example/calendars/"),
            "typed-for-caldav-example",
            "another path on its server is still its server"
        );
        for elsewhere in [
            "https://dav.elsewhere.example/dav/me/",
            "https://caldav.example:8443/dav/me/",
            "http://caldav.example/dav/me/",
        ] {
            assert_eq!(
                joined(elsewhere),
                "",
                "the password typed for https://caldav.example was joined to an account at \
                 {elsewhere}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    fn cfg(name: &str, user: &str) -> CalDavConfig {
        CalDavConfig {
            display_name: name.into(),
            server_url: "https://caldav.example".into(),
            username: user.into(),
            ..Default::default()
        }
    }

    fn temp_root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tilo-cal-accounts-{}", uuid::Uuid::new_v4()))
    }
}
