//! Multiple-account model and the shared, serialisable store the OS Settings
//! app reads and writes.
//!
//! ## Why a separate store
//! In qOS, account *management* (adding, editing, removing mail accounts,
//! entering credentials) lives in the **Settings app**, not in Mail — the
//! same way display, network, and Bluetooth settings do. Mail only *consumes*
//! the account list to fetch and send. To make that split real rather than
//! aspirational, the account list is modelled as a plain, `serde`-serialisable
//! [`AccountStore`] that both apps can read and write:
//!
//! * **Settings** owns the lifecycle: it loads the store, presents the
//!   add/edit UI, persists credentials to the OS secret store, and writes the
//!   store back.
//! * **Mail** loads the same store, shows the accounts in its sidebar, and
//!   drives IMAP/SMTP per account. Mail *also* offers a lightweight in-app
//!   add/edit form for now (so the app is usable before Settings ships the
//!   panel), writing through the identical [`AccountStore`] shape.
//!
//! The on-disk seam is a single JSON document at
//! [`AccountStore::SETTINGS_PATH`]; [`AccountStore::to_json`] /
//! [`AccountStore::from_json`] are the read/write entry points both apps share.
//! Credentials are deliberately **not** serialised here (see
//! [`AccountConfig`]'s note) — Settings stores those in the OS secret store and
//! injects them at connect time.

use serde::{Deserialize, Serialize};

use crate::model::{AccountConfig, Folder};

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
    /// Per-account so a user can keep work expanded and personal collapsed.
    #[serde(default)]
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
        Self { id, config, collapsed: false, tint: 0 }
    }

    /// Display label for the account header: the display name, falling back to
    /// the email, falling back to a generic placeholder.
    pub fn label(&self) -> &str {
        if !self.config.display_name.is_empty() {
            &self.config.display_name
        } else if !self.config.email.is_empty() {
            &self.config.email
        } else {
            "New account"
        }
    }
}

/// The shared, serialisable account list. This is the contract surface
/// between Mail and the Settings app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStore {
    pub accounts: Vec<Account>,
    /// Index of the account whose mailbox is currently shown. Clamped on read.
    #[serde(default)]
    pub active: usize,
}

impl Default for AccountStore {
    fn default() -> Self {
        Self { accounts: Vec::new(), active: 0 }
    }
}

impl AccountStore {
    /// The canonical on-disk location Settings writes and Mail reads. Lives
    /// under the user's config root (projected through the qOS FUSE layer in a
    /// real deployment). Kept as an associated const so both apps reference the
    /// same path symbol rather than a stray string literal.
    pub const SETTINGS_PATH: &'static str = "~/.config/qos/mail/accounts.json";

    pub fn new() -> Self {
        Self::default()
    }

    pub fn single(config: AccountConfig) -> Self {
        Self { accounts: vec![Account::new(config)], active: 0 }
    }

    /// Serialise the store for the Settings seam.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// Read a store written by Settings (or a previous Mail session).
    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
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
        self.active_account().map(|a| a.config.clone()).unwrap_or_default()
    }

    pub fn set_active(&mut self, idx: usize) {
        if idx < self.accounts.len() {
            self.active = idx;
        }
    }

    /// Add (or replace, by id) an account and make it active.
    pub fn upsert(&mut self, account: Account) {
        if let Some(idx) = self.accounts.iter().position(|a| a.id == account.id) {
            self.accounts[idx] = account;
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
    fn store_round_trips_through_json() {
        let mut store = AccountStore::new();
        store.upsert(Account::new(cfg("a@x.example", "A")));
        store.upsert(Account::new(cfg("b@y.example", "B")));
        store.set_active(0);
        let json = store.to_json();
        let back = AccountStore::from_json(&json).expect("valid json");
        assert_eq!(back.len(), 2);
        assert_eq!(back.active_account().unwrap().config.email, "a@x.example");
    }

    #[test]
    fn upsert_replaces_by_id_not_appends() {
        let mut store = AccountStore::new();
        store.upsert(Account::new(cfg("a@x.example", "A")));
        let mut updated = Account::new(cfg("a@x.example", "A renamed"));
        updated.id = "a@x.example".into();
        store.upsert(updated);
        assert_eq!(store.len(), 1);
        assert_eq!(store.accounts[0].config.display_name, "A renamed");
    }

    #[test]
    fn active_is_clamped_after_remove() {
        let mut store = AccountStore::new();
        store.upsert(Account::new(cfg("a@x.example", "A")));
        store.upsert(Account::new(cfg("b@y.example", "B")));
        store.set_active(1);
        store.remove("b@y.example");
        // active was 1, now only index 0 exists — must clamp, not panic.
        assert_eq!(store.active_account().unwrap().config.email, "a@x.example");
    }

    #[test]
    fn credentials_are_not_serialised() {
        let mut c = cfg("a@x.example", "A");
        c.password = "hunter2".into();
        let store = AccountStore::single(c);
        let json = store.to_json();
        assert!(
            !json.contains("hunter2"),
            "passwords must not be written to the shared settings store"
        );
    }
}
