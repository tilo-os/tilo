//! The calendar's own shape of the workspace's accounts: what a device with a
//! window reads them as. A calendar's password is the workspace's vault's, not
//! this device's: the app asks the session for it when it signs in.

use crate::Account as StoredAccount;
#[cfg(test)]
use crate::AccountId;
use crate::person::AccountList;
use serde::{Deserialize, Serialize};

use super::{CalDavConfig, CalendarAccount, CalendarAccounts, RemoteCalendar, TYPE_ID};

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
    fn from_stored(stored: &StoredAccount) -> Option<Self> {
        let payload: StoredConfig = stored
            .config
            .get("calendar")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .or_else(|| stored.config_as::<StoredConfig>())?;
        let mut config = payload.config;
        if config.display_name.is_empty() {
            config.display_name.clone_from(&stored.display_name);
        }
        // The entry of the workspace's vault this account's password is in, as Settings names it.
        // The app asks the session for it when it signs in, and this device keeps none.
        config.password_item = crate::email::password_item(&stored.config).map(str::to_owned);
        if stored
            .config
            .get("calendar_provider")
            .and_then(serde_json::Value::as_str)
            == Some("google")
        {
            config.oauth_account_id = Some(stored.id.to_string());
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

impl CalendarAccounts {
    /// The workspace's accounts as the calendar's shape, reading the record's calendar accounts and
    /// the Nextcloud accounts connected for calendars.
    pub fn from_shared(shared: &AccountList) -> Self {
        let accounts = shared
            .of_type(TYPE_ID)
            .into_iter()
            .chain(shared.all().into_iter().filter(|stored| {
                stored
                    .config
                    .get("use_calendar")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                    && stored.type_id.as_str() != TYPE_ID
            }))
            .filter(|stored| stored.enabled)
            .filter_map(|stored| CalendarAccount::from_stored(&stored))
            .collect();
        Self { accounts }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `account` as the record a workspace keeps: the config payload its holder writes for a
    /// calendar account, minus the credentials. The `#[serde(skip)]` password keeps the
    /// credential out of it; it lives only in the vault.
    fn stored(account: &CalendarAccount) -> StoredAccount {
        let mut kept = StoredAccount::new(AccountId::from(account.id.clone()), TYPE_ID);
        kept.display_name = account.label().to_string();
        kept.set_config(&StoredConfig {
            config: account.config.clone(),
            calendars: account.kept_calendars(),
            collapsed: account.collapsed,
            read_only: account.read_only,
        });
        kept
    }

    /// Keep `store`'s accounts as the record a home stages, as a device does
    /// through the workspace's record.
    fn keep(root: &std::path::Path, store: &CalendarAccounts) {
        let mut record = crate::person::staged_in(root).unwrap();
        for account in &store.accounts {
            record
                .apply_confirmed(AccountList::saving(&stored(account)))
                .unwrap();
        }
        crate::person::keep_staged_in(root, &mut record).unwrap();
    }

    /// The accounts `root`'s staged record holds, as the calendar reads them.
    fn load(root: &std::path::Path) -> CalendarAccounts {
        let record = crate::person::staged_in(root).unwrap();
        CalendarAccounts::from_shared(&record.state().unwrap())
    }

    fn cfg(name: &str, user: &str) -> CalDavConfig {
        CalDavConfig {
            display_name: name.into(),
            server_url: "https://caldav.example".into(),
            username: user.into(),
            ..Default::default()
        }
    }

    /// The shape Settings persists after a connect, minus the credentials.
    fn sample() -> CalendarAccounts {
        CalendarAccounts {
            accounts: vec![
                CalendarAccount {
                    id: "work@fastmail.example".into(),
                    config: CalDavConfig {
                        display_name: "Work".into(),
                        server_url: "https://caldav.fastmail.com/dav/calendars".into(),
                        username: "work@fastmail.example".into(),
                        password: String::new(),
                        password_item: None,
                        oauth_account_id: None,
                    },
                    calendars: vec![
                        RemoteCalendar {
                            id: "work/default".into(),
                            name: "Team".into(),
                            color: 0,
                            visible: true,
                            read_only: false,
                        },
                        RemoteCalendar {
                            id: "work/oncall".into(),
                            name: "On-call".into(),
                            color: 3,
                            visible: true,
                            read_only: false,
                        },
                    ],
                    collapsed: false,
                    read_only: false,
                },
                CalendarAccount {
                    id: "me@icloud.example".into(),
                    config: CalDavConfig {
                        display_name: "iCloud".into(),
                        server_url: "https://caldav.icloud.com".into(),
                        username: "me@icloud.example".into(),
                        password: String::new(),
                        password_item: None,
                        oauth_account_id: None,
                    },
                    calendars: vec![
                        RemoteCalendar {
                            id: "icloud/personal".into(),
                            name: "Personal".into(),
                            color: 1,
                            visible: true,
                            read_only: false,
                        },
                        RemoteCalendar {
                            id: "icloud/holidays".into(),
                            name: "Holidays".into(),
                            color: 2,
                            visible: false,
                            read_only: false,
                        },
                    ],
                    collapsed: false,
                    read_only: false,
                },
            ],
        }
    }

    fn temp_root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tilo-cal-accounts-{}", uuid::Uuid::new_v4()))
    }

    tilo_test_env::scenario! {
        /// The calendar's accounts: what it keeps in the workspace's record and what it
        /// reads back. No password is in it.
        fn a_calendar_accounts_password_stays_out_of_the_store;

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

        phase saved_calendar_accounts_load_again_with_their_calendars_and_which_show;
        {
            {
                let root = temp_root();
                let mut store = CalendarAccounts::new();
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
                keep(&root, &store);

                let back = load(&root);
                assert_eq!(back.len(), 2);
                assert_eq!(back.accounts[0].config.username, "a@x.example");
                assert_eq!(back.accounts[0].calendars.len(), 1);
                assert_eq!(back.accounts[0].calendars[0].name, "Team");
                assert!(back.accounts[0].collapsed);
                std::fs::remove_dir_all(&root).ok();
            }
            {
                let root = temp_root();
                // Another account of the workspace, naming the entry of the workspace's vault
                // its sign-in is in.
                let other_id = AccountId::from("unrelated-service");
                let mut other = StoredAccount::new(other_id.clone(), "unrelated");
                other.config = serde_json::json!({
                    crate::email::PASSWORD_ITEM_KEY: "vault.item:unrelated-sign-in",
                });
                let mut record = crate::person::staged_in(&root).unwrap();
                record
                    .apply_confirmed(AccountList::saving(&other))
                    .unwrap();
                crate::person::keep_staged_in(&root, &mut record).unwrap();

                let mut store = sample();
                keep(&root, &store);
                let calendar = &mut store.accounts[0].calendars[0];
                let id = calendar.id.clone();
                calendar.visible = false;
                keep(&root, &store);

                let restarted = load(&root);
                assert!(
                    restarted
                        .all_calendars()
                        .find(|(_, calendar)| calendar.id == id)
                        .is_some_and(|(_, calendar)| !calendar.visible)
                );
                let staged = crate::person::staged_in(&root).unwrap();
                let kept = staged.state().unwrap();
                let other = kept.account(other_id.as_str()).expect("still kept");
                assert_eq!(
                    crate::email::password_item(&other.config),
                    Some("vault.item:unrelated-sign-in"),
                    "saving the calendar's accounts keeps another account's sign-in named"
                );
                std::fs::remove_dir_all(&root).ok();
            }
            {
                let mut store = CalendarAccounts::new();
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
                let mut store = CalendarAccounts::new();
                store.upsert(CalendarAccount::new(c));
                keep(&root, &store);

                let kept =
                    String::from_utf8_lossy(&std::fs::read(crate::person::staged_path_in(&root)).unwrap())
                        .into_owned();
                assert!(
                    !kept.contains("hunter2"),
                    "a password must not reach the workspace's record"
                );

                let back = load(&root);
                assert_eq!(
                    back.accounts[0].config.password, "",
                    "a calendar's password is the workspace's vault's, never this device's"
                );
                std::fs::remove_dir_all(&root).ok();
            }
            {
                let store = sample();
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
            let mut store = CalendarAccounts::new();
            store.upsert(reading);
            store.upsert(writing);
            keep(&root, &store);

            // The account's settings say it, beside its server and its user name, and so
            // does every calendar of it. An account that is written to says nothing new.
            let reading_id = AccountId::from("a@x.example");
            let staged = crate::person::staged_in(&root).unwrap();
            let kept_record = staged.state().unwrap();
            let kept = kept_record.account(reading_id.as_str()).expect("kept").config;
            assert_eq!(kept["read_only"], true, "{kept}");
            assert_eq!(kept["calendars"][0]["read_only"], true, "{kept}");
            let other = kept_record.account("b@y.example").expect("kept").config;
            assert!(other.get("read_only").is_none(), "{other}");
            assert_eq!(other["calendars"][0]["read_only"], false, "{other}");

            // Read back, changed as a look at its server's calendars changes an account,
            // and saved again: the account's word stays, and no calendar of it is lifted.
            let mut back = load(&root);
            assert!(back.accounts[0].read_only && back.accounts[0].calendars[0].read_only);
            assert!(!back.accounts[1].read_only && !back.accounts[1].calendars[0].read_only);
            back.accounts[0].calendars = vec![team(false)];
            back.accounts[0].collapsed = true;
            keep(&root, &back);
            let again = load(&root);
            assert!(again.accounts[0].read_only && again.accounts[0].collapsed);
            assert!(again.accounts[0].calendars[0].read_only);
            assert!(!again.accounts[1].read_only && !again.accounts[1].calendars[0].read_only);

            // Settings in which a calendar lost its own mark and the account kept its word
            // read as an account none of whose calendars can be written.
            let mut record = crate::person::staged_in(&root).unwrap();
            let mut stored = record
                .state()
                .unwrap()
                .account(reading_id.as_str())
                .expect("kept");
            stored.config["calendars"][0]["read_only"] = false.into();
            record
                .apply_confirmed(AccountList::saving(&stored))
                .unwrap();
            crate::person::keep_staged_in(&root, &mut record).unwrap();
            let read = load(&root);
            assert!(read.accounts[0].read_only && read.accounts[0].calendars[0].read_only);
            std::fs::remove_dir_all(&root).ok();
        }
    }
}
