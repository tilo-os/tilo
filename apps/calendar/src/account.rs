//! Calendar's view of the shared multi-account store.
//!
//! ## One shared store, one vault
//! Account *management* (adding, editing, removing calendar accounts,
//! entering credentials) lives in the **Settings app**, not in the Calendar —
//! the same way display, network, and Bluetooth settings do, and the same way
//! the Mail app consumes its accounts. The canonical store is the shared
//! [`accounts`] crate: a single JSON document under
//! `$XDG_CONFIG_HOME/tilo/accounts/` holding every account of every type, with
//! credentials sealed in the encrypted [`accounts::SecretVault`], never in the
//! plaintext store.
//!
//! Historically Calendar and Settings each declared their own store *shape*
//! and disagreed (Settings persisted a `DavAccount` layout, Calendar expected
//! `CalendarAccount`), so an account added in one was invisible to the other.
//! Reading and writing the one shared store resolves that by construction —
//! both sides now (de)serialise the same [`accounts::Account`], with the
//! `CalDAV` settings carried in its opaque `config`.

use std::path::Path;

use accounts::{
    Account as StoredAccount, AccountId, AccountStore as SharedStore, AccountTypeId, SecretVault,
};
use serde::{Deserialize, Serialize};

use crate::model::Palette;

/// The account-type id Calendar contributes to the shared store.
pub const TYPE_ID: &str = "caldav";

/// `CalDAV` connection + identity settings — exactly what the setup form
/// collects and the sync layer consumes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CalDavConfig {
    /// Friendly account name shown in the sidebar header ("Work", "Fastmail").
    pub display_name: String,
    /// Base `CalDAV` server URL. The well-known discovery path
    /// (`/.well-known/caldav`) and principal/home-set resolution are handled by
    /// the sync layer; this is the value the user types.
    pub server_url: String,
    /// Login name for the `CalDAV` server (often the email address).
    pub username: String,
    /// Held only in memory here and hydrated from the [`accounts::SecretVault`]
    /// on load; the sync layer uses it at connect time. Never written to the
    /// plaintext store (hence `serde(skip)`) — it lives only in the vault.
    #[serde(skip)]
    pub password: String,
}

impl CalDavConfig {
    /// Heuristic autofill: given a username/email, guess a well-known `CalDAV`
    /// endpoint for the big providers. Mirrors what a desktop client's
    /// autoconfig does. Returns true if it recognised the domain.
    pub fn autofill_from_username(&mut self) -> bool {
        let Some((_, domain)) = self.username.split_once('@') else {
            return false;
        };
        let url = match domain.to_ascii_lowercase().as_str() {
            "gmail.com" | "googlemail.com" => "https://apidata.googleusercontent.com/caldav/v2",
            "icloud.com" | "me.com" => "https://caldav.icloud.com",
            "fastmail.com" | "fastmail.fm" => "https://caldav.fastmail.com/dav/calendars",
            "yahoo.com" => "https://caldav.calendar.yahoo.com",
            other => {
                self.server_url = format!("https://{other}/.well-known/caldav");
                return false;
            }
        };
        self.server_url = url.to_string();
        true
    }

    /// Field-level validation for the setup form: returns `(field, reason)`
    /// pairs for anything missing or malformed. Empty vec == valid.
    pub fn validate(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if self.display_name.trim().is_empty() {
            out.push(("display_name", "Give the account a name".into()));
        }
        if self.server_url.trim().is_empty() {
            out.push(("server_url", "Server URL is required".into()));
        } else if !self.server_url.starts_with("http://")
            && !self.server_url.starts_with("https://")
        {
            out.push(("server_url", "Server URL must start with http(s)://".into()));
        }
        if self.username.trim().is_empty() {
            out.push(("username", "Username is required".into()));
        }
        out
    }

    pub fn is_valid(&self) -> bool {
        self.validate().is_empty()
    }
}

/// One remote calendar collection within an account (a `CalDAV` `calendar-home-set`
/// child). Each is an independent sync target with its own visibility toggle and
/// display colour, so a user can show "Work" but hide "Holidays".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCalendar {
    /// Stable id — in a live deployment the collection href; here a slug.
    pub id: String,
    /// Calendar display name from the `CalDAV` `displayname` property.
    pub name: String,
    /// Which palette slot tints this calendar's events. Stored as the palette
    /// index so the JSON stays OS-theme-agnostic.
    #[serde(default)]
    pub color: u8,
    /// Whether this calendar's events are shown. Off == synced but hidden.
    #[serde(default = "default_true")]
    pub visible: bool,
}

fn default_true() -> bool {
    true
}

impl RemoteCalendar {
    pub fn palette(&self) -> Palette {
        Palette::from_index(self.color as usize)
    }
}

/// A single configured calendar account: its `CalDAV` connection settings, the
/// calendars discovered within it, plus the per-account UI state the Calendar
/// app needs (sidebar expand/collapse).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarAccount {
    /// Stable id (the username at creation time is a fine natural key, but we
    /// keep an explicit field so a later rename doesn't reshuffle the store).
    pub id: String,
    pub config: CalDavConfig,
    /// Calendars discovered under this account's home-set. Settings refreshes
    /// this on connect; Calendar reads it to populate the sidebar.
    #[serde(default)]
    pub calendars: Vec<RemoteCalendar>,
    /// Whether this account's calendar list is collapsed in the sidebar.
    #[serde(default)]
    pub collapsed: bool,
}

impl CalendarAccount {
    pub fn new(config: CalDavConfig) -> Self {
        let id = if config.username.is_empty() {
            format!("account-{}", uuid::Uuid::new_v4())
        } else {
            config.username.clone()
        };
        Self {
            id,
            config,
            calendars: Vec::new(),
            collapsed: false,
        }
    }

    /// Display label for the account header: the display name, falling back to
    /// the username, falling back to a generic placeholder.
    pub fn label(&self) -> &str {
        if !self.config.display_name.is_empty() {
            &self.config.display_name
        } else if !self.config.username.is_empty() {
            &self.config.username
        } else {
            "New account"
        }
    }

    /// The opaque, secret-free config payload persisted into a shared
    /// [`StoredAccount`]. The `#[serde(skip)]` password keeps the credential
    /// out of this JSON; it lives only in the vault.
    fn to_stored(&self) -> StoredAccount {
        let mut stored = StoredAccount::new(AccountId::from(self.id.clone()), TYPE_ID);
        stored.display_name = self.label().to_string();
        stored.set_config(&StoredConfig {
            config: self.config.clone(),
            calendars: self.calendars.clone(),
            collapsed: self.collapsed,
        });
        stored
    }

    /// Rebuild a calendar account from a shared [`StoredAccount`], reading the
    /// password back from `vault`.
    fn from_stored(stored: &StoredAccount, vault: &SecretVault) -> Option<Self> {
        let payload = stored.config_as::<StoredConfig>()?;
        let mut config = payload.config;
        if let Some(secret) = vault.get(&stored.id) {
            config.password = secret.password;
        }
        Some(Self {
            id: stored.id.to_string(),
            config,
            calendars: payload.calendars,
            collapsed: payload.collapsed,
        })
    }
}

/// The opaque config payload a [`CalendarAccount`] serialises into a shared
/// [`StoredAccount`]: the `CalDAV` settings (password skipped), the discovered
/// calendars, and the sidebar collapse state.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredConfig {
    #[serde(flatten)]
    config: CalDavConfig,
    #[serde(default)]
    calendars: Vec<RemoteCalendar>,
    #[serde(default)]
    collapsed: bool,
}

/// Calendar's in-memory account list, projected from the shared store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountStore {
    pub accounts: Vec<CalendarAccount>,
}

impl AccountStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    /// Add (or replace, by id) an account.
    pub fn upsert(&mut self, account: CalendarAccount) {
        if let Some(slot) = self.accounts.iter_mut().find(|a| a.id == account.id) {
            *slot = account;
        } else {
            self.accounts.push(account);
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.accounts.retain(|a| a.id != id);
    }

    /// Every visible calendar across every account, in account-then-calendar
    /// order. The sidebar's calendar checklist iterates this.
    pub fn all_calendars(&self) -> impl Iterator<Item = (&CalendarAccount, &RemoteCalendar)> {
        self.accounts
            .iter()
            .flat_map(|a| a.calendars.iter().map(move |c| (a, c)))
    }

    /// Load the `CalDAV` accounts the OS Settings app maintains from the shared
    /// canonical store, hydrating each password from the secret vault. Returns
    /// `None` only when no config root is discoverable; an empty store
    /// otherwise, so the app runs on local + sample data.
    pub fn load_from_settings() -> Option<Self> {
        let root = accounts::config_root()?;
        Some(Self::load_from_settings_in(&root))
    }

    /// Persist Calendar's account projection to the canonical shared store.
    /// Other account types and their sealed secrets are retained by
    /// [`Self::persist_in`]; callers receive an error when the real config
    /// root cannot be resolved or written, rather than claiming a setting was
    /// saved when it was not.
    ///
    /// # Errors
    ///
    /// Returns `Err` when no Tilo config root is available or persistence fails.
    pub fn persist(&self) -> std::io::Result<()> {
        let root = accounts::config_root()
            .ok_or_else(|| std::io::Error::other("no config dir for calendar accounts"))?;
        self.persist_in(&root)
    }

    /// Load under an explicit config root — the seam tests use.
    pub fn load_from_settings_in(config_root: &Path) -> Self {
        Self::from_shared(
            &SharedStore::load_in(config_root),
            &SecretVault::open_in(config_root),
        )
    }

    fn from_shared(shared: &SharedStore, vault: &SecretVault) -> Self {
        let ty = AccountTypeId::from(TYPE_ID);
        let accounts = shared
            .list(&ty)
            .into_iter()
            .filter_map(|stored| CalendarAccount::from_stored(stored, vault))
            .collect();
        Self { accounts }
    }

    /// Write these accounts into the shared store + vault under an explicit
    /// config root, preserving accounts of other types already there. The
    /// write half of the seam (Settings owns the lifecycle; this exists so the
    /// round-trip is testable).
    ///
    /// # Errors
    ///
    /// Returns an error if the shared account store cannot be saved to
    /// `config_root` (for example, if the directory cannot be created or
    /// written to).
    pub fn persist_in(&self, config_root: &Path) -> std::io::Result<()> {
        let mut shared = SharedStore::load_in(config_root);
        let mut vault = SecretVault::open_in(config_root);
        for acct in &self.accounts {
            let id = AccountId::from(acct.id.clone());
            shared.upsert(acct.to_stored());
            if acct.config.password.is_empty() {
                vault.remove(&id);
            } else {
                vault.put(
                    &id,
                    &accounts::Secret::password(acct.config.password.clone()),
                );
            }
        }
        shared.save_in(config_root)?;
        vault.save();
        Ok(())
    }

    /// A representative offline store so `--shots` shows real `CalDAV` accounts in
    /// the sidebar without a network or a configured machine. These mirror the
    /// shape Settings would persist after a successful connect — minus the
    /// credentials, which never live here.
    pub fn sample() -> Self {
        Self {
            accounts: vec![
                CalendarAccount {
                    id: "work@fastmail.example".into(),
                    config: CalDavConfig {
                        display_name: "Work".into(),
                        server_url: "https://caldav.fastmail.com/dav/calendars".into(),
                        username: "work@fastmail.example".into(),
                        password: String::new(),
                    },
                    calendars: vec![
                        RemoteCalendar {
                            id: "work/default".into(),
                            name: "Team".into(),
                            color: Palette::Indigo.index() as u8,
                            visible: true,
                        },
                        RemoteCalendar {
                            id: "work/oncall".into(),
                            name: "On-call".into(),
                            color: Palette::Red.index() as u8,
                            visible: true,
                        },
                    ],
                    collapsed: false,
                },
                CalendarAccount {
                    id: "me@icloud.example".into(),
                    config: CalDavConfig {
                        display_name: "iCloud".into(),
                        server_url: "https://caldav.icloud.com".into(),
                        username: "me@icloud.example".into(),
                        password: String::new(),
                    },
                    calendars: vec![
                        RemoteCalendar {
                            id: "icloud/personal".into(),
                            name: "Personal".into(),
                            color: Palette::Emerald.index() as u8,
                            visible: true,
                        },
                        RemoteCalendar {
                            id: "icloud/holidays".into(),
                            name: "Holidays".into(),
                            color: Palette::Amber.index() as u8,
                            visible: false,
                        },
                    ],
                    collapsed: false,
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn account_store_round_trips_accounts_visibility_and_vaulted_secrets() {
        // Case `shared_store_round_trips_accounts_and_calendars`.
        {
            let root = temp_root();
            let mut store = AccountStore::new();
            let mut a = CalendarAccount::new(cfg("Work", "a@x.example"));
            a.calendars.push(RemoteCalendar {
                id: "work/team".into(),
                name: "Team".into(),
                color: 1,
                visible: true,
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
        // Case `visibility_round_trip_preserves_unrelated_accounts_and_secrets`.
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
        // Case `upsert_replaces_by_id_not_appends`.
        {
            let mut store = AccountStore::new();
            store.upsert(CalendarAccount::new(cfg("A", "a@x.example")));
            let mut updated = CalendarAccount::new(cfg("A renamed", "a@x.example"));
            updated.id = "a@x.example".into();
            store.upsert(updated);
            assert_eq!(store.len(), 1);
            assert_eq!(store.accounts[0].config.display_name, "A renamed");
        }
        // Case `password_persists_to_the_vault_not_the_store_json`.
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
        // Case `sample_store_has_accounts_and_calendars`.
        {
            let store = AccountStore::sample();
            assert_eq!(store.len(), 2);
            // Two visible + one hidden calendar across the sample accounts.
            let total = store.all_calendars().count();
            assert_eq!(total, 4);
            let visible = store.all_calendars().filter(|(_, c)| c.visible).count();
            assert_eq!(visible, 3);
        }
    }

    #[test]
    fn config_autofill_recognises_providers_and_validation_flags_missing_fields() {
        // Case `autofill_recognises_known_provider`.
        {
            let mut c = CalDavConfig {
                username: "a@icloud.com".into(),
                ..Default::default()
            };
            assert!(c.autofill_from_username());
            assert!(c.server_url.contains("icloud.com"));
        }
        // Case `validate_flags_missing_fields`.
        {
            let c = CalDavConfig::default();
            let errs = c.validate();
            assert!(errs.iter().any(|(f, _)| *f == "server_url"));
            assert!(errs.iter().any(|(f, _)| *f == "username"));
        }
    }
}
