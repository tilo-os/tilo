//! The calendar's shape of the workspace's accounts: the servers it signs in
//! to and the calendars each of them holds.
//!
//! The types are the account record's, read by an agent too. Reading the
//! workspace's account list in this shape, with a password from the vault, is
//! [`CalendarAccounts::from_shared`].

use serde::{Deserialize, Serialize};

pub const TYPE_ID: &str = "caldav";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CalDavConfig {
    #[serde(default)]
    pub display_name: String,
    pub server_url: String,
    pub username: String,
    /// Held only in memory here, filled from the session when the account signs in. Never
    /// written to the plaintext store (hence `serde(skip)`): the password is the workspace's
    /// vault's.
    #[serde(skip)]
    pub password: String,
    /// The entry of the workspace's vault the password is in, as Settings names it: the app asks
    /// the session for it when it signs in, and holds it in memory only.
    #[serde(skip)]
    pub password_item: Option<String>,
    #[serde(skip)]
    pub oauth_account_id: Option<String>,
}

impl std::fmt::Debug for CalDavConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalDavConfig").finish_non_exhaustive()
    }
}

impl CalDavConfig {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCalendar {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: u8,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub read_only: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarAccount {
    pub id: String,
    pub config: CalDavConfig,
    #[serde(default)]
    pub calendars: Vec<RemoteCalendar>,
    #[serde(default)]
    pub collapsed: bool,
    /// Whether the account is connected for reading only: nothing is ever
    /// written to its calendar server, by anybody. Every calendar of such an
    /// account is kept as one that cannot be written, whatever its server
    /// would let the person do there ([`Self::kept_calendars`]).
    #[serde(default)]
    pub read_only: bool,
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
            read_only: false,
        }
    }

    pub fn label(&self) -> &str {
        if !self.config.display_name.is_empty() {
            &self.config.display_name
        } else if !self.config.username.is_empty() {
            &self.config.username
        } else {
            "New account"
        }
    }

    /// The account's calendars as they are kept and read: each one read
    /// only where the account is connected for reading only. The account's
    /// word decides, so a look at what its server holds, which answers what
    /// that server would let the person do, lifts none of them.
    #[must_use]
    pub fn kept_calendars(&self) -> Vec<RemoteCalendar> {
        self.calendars
            .iter()
            .cloned()
            .map(|mut calendar| {
                calendar.read_only |= self.read_only;
                calendar
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CalendarAccounts {
    pub accounts: Vec<CalendarAccount>,
}

impl CalendarAccounts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn upsert(&mut self, account: CalendarAccount) {
        if let Some(held) = self.accounts.iter_mut().find(|a| a.id == account.id) {
            *held = account;
        } else {
            self.accounts.push(account);
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.accounts.retain(|a| a.id != id);
    }

    pub fn all_calendars(&self) -> impl Iterator<Item = (&CalendarAccount, &RemoteCalendar)> {
        self.accounts
            .iter()
            .flat_map(|a| a.calendars.iter().map(move |c| (a, c)))
    }
}

#[cfg(feature = "person")]
mod store;
