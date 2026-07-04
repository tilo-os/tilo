//! Calendar accounts and the shared, serialisable store the OS Settings app
//! reads and writes.
//!
//! ## Why a separate store
//! In qOS, account *management* (adding, editing, removing calendar accounts,
//! entering credentials) lives in the **Settings app**, not in the Calendar —
//! the same way display, network, and Bluetooth settings do, and the same way
//! the Mail app consumes its own account store. To make that split real rather
//! than aspirational, the account list is modelled as a plain,
//! `serde`-serialisable [`AccountStore`] that both apps read and write:
//!
//! * **Settings** owns the lifecycle: it loads the store, presents the
//!   add/edit UI (CalDAV server URL, username), persists credentials to the OS
//!   secret store, discovers each account's calendar collections, and writes
//!   the store back.
//! * **Calendar** loads the same store, lists each account's calendars in its
//!   sidebar, and drives CalDAV sync per calendar (see [`crate::caldav`]).
//!
//! The on-disk seam is a single JSON document at [`AccountStore::SETTINGS_PATH`];
//! [`AccountStore::to_json`] / [`AccountStore::from_json`] are the read/write
//! entry points both apps share. Credentials are deliberately **not** serialised
//! here (see [`CalDavConfig`]'s note) — Settings stores those in the OS secret
//! store and injects them at connect time.

use serde::{Deserialize, Serialize};

use crate::model::Palette;

/// CalDAV connection + identity settings — exactly what the setup form
/// collects and the sync layer consumes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalDavConfig {
    /// Friendly account name shown in the sidebar header ("Work", "Fastmail").
    pub display_name: String,
    /// Base CalDAV server URL. The well-known discovery path
    /// (`/.well-known/caldav`) and principal/home-set resolution are handled by
    /// the sync layer; this is the value the user types.
    pub server_url: String,
    /// Login name for the CalDAV server (often the email address).
    pub username: String,
    /// Stored only in memory in this slice; a real deployment pushes this to
    /// the OS secret store. Never written to the shared settings JSON (the
    /// Settings app injects it at connect time) — hence `serde(skip)`.
    #[serde(skip)]
    pub password: String,
}

impl Default for CalDavConfig {
    fn default() -> Self {
        Self {
            display_name: String::new(),
            server_url: String::new(),
            username: String::new(),
            password: String::new(),
        }
    }
}

impl CalDavConfig {
    /// Heuristic autofill: given a username/email, guess a well-known CalDAV
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
        } else if !self.server_url.starts_with("http://") && !self.server_url.starts_with("https://")
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

/// One remote calendar collection within an account (a CalDAV `calendar-home-set`
/// child). Each is an independent sync target with its own visibility toggle and
/// display colour, so a user can show "Work" but hide "Holidays".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCalendar {
    /// Stable id — in a live deployment the collection href; here a slug.
    pub id: String,
    /// Calendar display name from the CalDAV `displayname` property.
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

/// A single configured calendar account: its CalDAV connection settings, the
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
        Self { id, config, calendars: Vec::new(), collapsed: false }
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
}

/// The shared, serialisable account list. This is the contract surface between
/// Calendar and the Settings app.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStore {
    pub accounts: Vec<CalendarAccount>,
}

impl AccountStore {
    /// The canonical on-disk location Settings writes and Calendar reads. Lives
    /// under the user's config root (projected through the qOS FUSE layer in a
    /// real deployment). Kept as an associated const so both apps reference the
    /// same path symbol rather than a stray string literal.
    pub const SETTINGS_PATH: &'static str = "~/.config/qos/calendar/accounts.json";

    pub fn new() -> Self {
        Self::default()
    }

    /// Serialise the store for the Settings seam.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// Read a store written by Settings (or a previous Calendar session).
    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
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

    /// Load the store the OS Settings app maintains. Returns `None` if no store
    /// exists yet (a fresh machine with no accounts configured), in which case
    /// the app runs purely on local + sample data.
    ///
    /// `SETTINGS_PATH` is a `~`-relative path resolved against `$HOME`; the
    /// real path is projected through the qOS FUSE config root.
    pub fn load_from_settings() -> Option<Self> {
        let path = expand_home(Self::SETTINGS_PATH)?;
        let raw = std::fs::read_to_string(path).ok()?;
        Self::from_json(&raw)
    }

    /// A representative offline store so `--shots` shows real CalDAV accounts in
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

/// Resolve a leading `~` against `$HOME`. Returns `None` if `$HOME` is unset and
/// the path needs it.
fn expand_home(path: &str) -> Option<std::path::PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var_os("HOME")?;
        Some(std::path::Path::new(&home).join(rest))
    } else {
        Some(std::path::PathBuf::from(path))
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

    #[test]
    fn store_round_trips_through_json() {
        let mut store = AccountStore::new();
        store.upsert(CalendarAccount::new(cfg("A", "a@x.example")));
        store.upsert(CalendarAccount::new(cfg("B", "b@y.example")));
        let json = store.to_json();
        let back = AccountStore::from_json(&json).expect("valid json");
        assert_eq!(back.len(), 2);
        assert_eq!(back.accounts[0].config.username, "a@x.example");
    }

    #[test]
    fn upsert_replaces_by_id_not_appends() {
        let mut store = AccountStore::new();
        store.upsert(CalendarAccount::new(cfg("A", "a@x.example")));
        let mut updated = CalendarAccount::new(cfg("A renamed", "a@x.example"));
        updated.id = "a@x.example".into();
        store.upsert(updated);
        assert_eq!(store.len(), 1);
        assert_eq!(store.accounts[0].config.display_name, "A renamed");
    }

    #[test]
    fn credentials_are_not_serialised() {
        let mut c = cfg("A", "a@x.example");
        c.password = "hunter2".into();
        let store = AccountStore { accounts: vec![CalendarAccount::new(c)] };
        let json = store.to_json();
        assert!(
            !json.contains("hunter2"),
            "passwords must not be written to the shared settings store"
        );
    }

    #[test]
    fn sample_store_has_accounts_and_calendars() {
        let store = AccountStore::sample();
        assert_eq!(store.len(), 2);
        // Two visible + one hidden calendar across the sample accounts.
        let total = store.all_calendars().count();
        assert_eq!(total, 4);
        let visible = store.all_calendars().filter(|(_, c)| c.visible).count();
        assert_eq!(visible, 3);
    }

    #[test]
    fn autofill_recognises_known_provider() {
        let mut c = CalDavConfig { username: "a@icloud.com".into(), ..Default::default() };
        assert!(c.autofill_from_username());
        assert!(c.server_url.contains("icloud.com"));
    }

    #[test]
    fn validate_flags_missing_fields() {
        let c = CalDavConfig::default();
        let errs = c.validate();
        assert!(errs.iter().any(|(f, _)| *f == "server_url"));
        assert!(errs.iter().any(|(f, _)| *f == "username"));
    }
}
