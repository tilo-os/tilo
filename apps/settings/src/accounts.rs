//! The shared **external-accounts stores** — the on-disk contracts the
//! Settings "External Accounts" panel edits and the Mail / Calendar / file
//! apps consume.
//!
//! # Why Settings owns these
//!
//! Account inventory in qOS is presented in one place: this app's External
//! Accounts panel. Password/app-password accounts for Mail, `CalDAV`, `WebDAV`,
//! and Nextcloud can be added, edited, tested, reauthenticated, and removed
//! there. Provider-specific OAuth authorization remains in its owning app.
//!
//! # Where email + `CalDAV` live: the shared `accounts` crate
//!
//! Email and `CalDAV` accounts are persisted through the shared
//! [`accounts::AccountStore`] — one canonical JSON document at
//! `~/.config/qos/accounts/store.json` — the **same** store Mail and Calendar
//! read. Settings owns the write lifecycle (add / edit / remove); those apps
//! consume it. Passwords are sealed in the accompanying encrypted
//! [`accounts::SecretVault`], never in the plaintext store.
//!
//! [`AccountDirectory::load`] projects that shared store into the in-memory
//! [`MailStore`] / [`DavStore`] shapes this pane renders, so the pane code is
//! unchanged while the on-disk store is the shared one. Consequently the
//! `MailStore` / `MailAccount` / `AccountConfig` types below are now **render
//! view-models** populated from the shared store — the legacy
//! `MailStore::path` / `MailStore::save` remain only as legacy-format helpers;
//! every account kind now writes the canonical store.
//!
//! # Legacy migration
//!
//! Older installs kept `WebDAV` and Nextcloud metadata in per-app JSON files.
//! [`AccountDirectory::load`] imports those records into the canonical store
//! once (upsert by stable id), leaving the old files untouched as a rollback
//! aid. New writes only target the canonical store.
//!
//! # Credentials are never serialised
//!
//! Passwords / tokens live in the OS secret store, not these JSON files.
//! Every credential field is `#[serde(skip)]`. Settings persists secrets
//! separately keyed by the account id; consumers ask the secret store for
//! them at connect time.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountMutationJournal {
    #[serde(rename = "previous_store")]
    store: accounts::AccountStore,
    #[serde(rename = "previous_statuses")]
    statuses: accounts::AccountSyncStatusStore,
    /// Previous vault ciphertext, never plaintext credential material.
    #[serde(rename = "previous_vault_ciphertext")]
    vault_ciphertext: Option<Vec<u8>>,
}

fn mutation_journal_path() -> Result<PathBuf, String> {
    let root = accounts::config_root().ok_or_else(|| "no config dir".to_string())?;
    Ok(accounts::accounts_dir_in(&root).join("mutation-journal.json"))
}

/// # Errors
///
/// Returns an error when the recovery check or durable journal write fails.
pub fn begin_account_mutation(
    previous_store: &accounts::AccountStore,
    previous_statuses: &accounts::AccountSyncStatusStore,
) -> Result<(), String> {
    recover_account_mutation()?;
    let root = accounts::config_root().ok_or_else(|| "no config dir".to_string())?;
    let vault_ciphertext = std::fs::read(accounts::vault_dir_in(&root).join("vault.enc")).ok();
    let journal = AccountMutationJournal {
        store: previous_store.clone(),
        statuses: previous_statuses.clone(),
        vault_ciphertext,
    };
    let bytes = serde_json::to_vec(&journal)
        .map_err(|error| format!("could not serialize account transaction: {error}"))?;
    atomic_file::write_creating_parent_with_mode(&mutation_journal_path()?, &bytes, 0o600)
        .map_err(|error| format!("could not persist account transaction intent: {error}"))
}

/// # Errors
///
/// Returns an error when the completed mutation journal cannot be removed.
pub fn finish_account_mutation() -> Result<(), String> {
    let path = mutation_journal_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => {
            if let Some(parent) = path.parent()
                && let Ok(directory) = std::fs::File::open(parent)
            {
                let _ = directory.sync_all();
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not clear account transaction intent: {error}"
        )),
    }
}

/// Roll back an interrupted metadata/vault/status mutation before presenting
/// the account inventory. The journal contains ciphertext, never a plaintext
/// password or token.
///
/// # Errors
///
/// Returns an error when the journal or the recovered stores cannot be read
/// or durably restored.
pub fn recover_account_mutation() -> Result<(), String> {
    let path = mutation_journal_path()?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "could not read account transaction intent: {error}"
            ));
        }
    };
    let journal: AccountMutationJournal = serde_json::from_slice(&bytes)
        .map_err(|error| format!("account transaction intent is invalid: {error}"))?;
    journal
        .store
        .save()
        .map_err(|error| format!("could not recover account metadata: {error}"))?;
    journal
        .statuses
        .save()
        .map_err(|error| format!("could not recover account sync status: {error}"))?;
    let root = accounts::config_root().ok_or_else(|| "no config dir".to_string())?;
    let vault_path = accounts::vault_dir_in(&root).join("vault.enc");
    match journal.vault_ciphertext {
        Some(ciphertext) => {
            atomic_file::write_creating_parent_with_mode(&vault_path, &ciphertext, 0o600)
                .map_err(|error| format!("could not recover credential vault: {error}"))?;
        }
        None => match std::fs::remove_file(&vault_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not recover credential vault: {error}")),
        },
    }
    finish_account_mutation()
}

// ---------------------------------------------------------------------------
// Mail store — byte-compatible with Mail's `~/.config/qos/mail/accounts.json`.
// ---------------------------------------------------------------------------

/// IMAP receive + SMTP send settings. Field-for-field the shape Mail's
/// transport layer consumes. `password` is `#[serde(skip)]` — it never
/// touches the JSON; Settings stores it in the OS secret store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountConfig {
    pub display_name: String,
    pub email: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: bool,
    pub username: String,
    #[serde(skip)]
    pub password: String,
}

impl Default for AccountConfig {
    fn default() -> Self {
        Self {
            display_name: String::new(),
            email: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            imap_tls: true,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_tls: true,
            username: String::new(),
            password: String::new(),
        }
    }
}

impl AccountConfig {
    /// Provider autoconfig: given an email, guess IMAP/SMTP hosts for the
    /// big providers — mirrors Thunderbird's autoconfig. Returns whether
    /// the domain was recognised.
    pub fn autofill_from_email(&mut self) -> bool {
        let Some((local, domain)) = self.email.split_once('@') else {
            return false;
        };
        if self.username.is_empty() {
            self.username = self.email.clone();
        }
        if self.display_name.is_empty() {
            self.display_name = local.to_string();
        }
        let known = match domain.to_ascii_lowercase().as_str() {
            "gmail.com" | "googlemail.com" => Some(("imap.gmail.com", "smtp.gmail.com")),
            "outlook.com" | "hotmail.com" | "live.com" => {
                Some(("outlook.office365.com", "smtp.office365.com"))
            }
            "yahoo.com" => Some(("imap.mail.yahoo.com", "smtp.mail.yahoo.com")),
            "icloud.com" | "me.com" => Some(("imap.mail.me.com", "smtp.mail.me.com")),
            _ => None,
        };
        if let Some((imap, smtp)) = known {
            self.imap_host = imap.to_string();
            self.smtp_host = smtp.to_string();
            true
        } else {
            self.imap_host = format!("imap.{domain}");
            self.smtp_host = format!("smtp.{domain}");
            false
        }
    }
}

/// A single configured mail account — Mail's `Account` shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailAccount {
    pub id: String,
    pub config: AccountConfig,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub tint: u8,
}

impl MailAccount {
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

    /// Display label: display name → email → placeholder.
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

/// The mail account list — Mail's `AccountStore` shape, persisted at
/// [`MailStore::PATH`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailStore {
    pub accounts: Vec<MailAccount>,
    #[serde(default)]
    pub active: usize,
}

impl MailStore {
    /// The path Mail reads and Settings writes, relative to the config
    /// root: `<config>/qos/mail/accounts.json`.
    pub const REL_PATH: &'static str = "mail/accounts.json";

    pub fn path() -> Option<PathBuf> {
        config_root().map(|r| r.join("mail").join("accounts.json"))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| Self::from_json(&s))
            .unwrap_or_default()
    }

    /// # Errors
    ///
    /// Returns an error if no config directory can be resolved (`no config
    /// dir`), or if creating the parent directory or writing the JSON file
    /// fails (permissions, missing/read-only filesystem, etc).
    pub fn save(&self) -> std::io::Result<PathBuf> {
        let path = Self::path().ok_or_else(|| std::io::Error::other("no config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, self.to_json())?;
        Ok(path)
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.accounts.len();
        self.accounts.retain(|a| a.id != id);
        self.accounts.len() != before
    }
}

// ---------------------------------------------------------------------------
// DAV store — CalDAV (calendar) and WebDAV (files), parallel files.
// ---------------------------------------------------------------------------

/// A `CalDAV` / `WebDAV` account. Both speak HTTP(S) to a base URL with a
/// username, so they share one shape. Credentials are never serialised.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DavAccount {
    pub id: String,
    pub label: String,
    /// Collection base URL, e.g. `https://dav.example.com/calendars/me/`.
    pub base_url: String,
    pub username: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub tint: u8,
    #[serde(skip)]
    pub password: String,
}

impl DavAccount {
    pub fn new(
        label: impl Into<String>,
        base_url: impl Into<String>,
        username: impl Into<String>,
    ) -> Self {
        Self {
            id: format!("dav-{}", uuid::Uuid::new_v4()),
            label: label.into(),
            base_url: base_url.into(),
            username: username.into(),
            enabled: true,
            tint: 0,
            password: String::new(),
        }
    }
}

/// A DAV account list. The *same* struct backs both the calendar store
/// (`<config>/qos/calendar/accounts.json`, consumed by Calendar) and the
/// files store (`<config>/qos/files/accounts.json`, consumed by the file
/// apps) — they differ only by path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DavStore {
    #[serde(default = "default_version")]
    pub version: u32,
    pub accounts: Vec<DavAccount>,
}

impl DavStore {
    /// The `CalDAV` (calendar) store path.
    pub fn calendar_path() -> Option<PathBuf> {
        config_root().map(|r| r.join("calendar").join("accounts.json"))
    }

    /// The `WebDAV` (files) store path.
    pub fn files_path() -> Option<PathBuf> {
        config_root().map(|r| r.join("files").join("accounts.json"))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }

    pub fn load_from(path: Option<PathBuf>) -> Self {
        path.and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| Self::from_json(&s))
            .unwrap_or_default()
    }

    /// # Errors
    ///
    /// Returns an error if `path` is `None` (`no config dir`), or if creating
    /// the parent directory or writing the JSON file fails (permissions,
    /// missing/read-only filesystem, etc).
    pub fn save_to(&self, path: Option<PathBuf>) -> std::io::Result<PathBuf> {
        let path = path.ok_or_else(|| std::io::Error::other("no config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, self.to_json())?;
        Ok(path)
    }
}

// ---------------------------------------------------------------------------
// Nextcloud store — one sign-in lighting up mail/calendar/files.
// ---------------------------------------------------------------------------

/// A Nextcloud account: a server URL + user that can surface mail,
/// calendar, and files. Consumers derive per-service URLs from the root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independently toggled service (files/calendar/mail) plus \
              enabled, all serialised individually; not a state machine to collapse into an enum"
)]
pub struct NextcloudAccount {
    pub id: String,
    pub label: String,
    /// Server root, e.g. `https://cloud.example.com`.
    pub server_url: String,
    pub username: String,
    #[serde(default = "default_true")]
    pub use_files: bool,
    #[serde(default = "default_true")]
    pub use_calendar: bool,
    #[serde(default = "default_true")]
    pub use_mail: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub tint: u8,
    #[serde(skip)]
    pub password: String,
}

impl NextcloudAccount {
    pub fn new(
        label: impl Into<String>,
        server_url: impl Into<String>,
        username: impl Into<String>,
    ) -> Self {
        Self {
            id: format!("nc-{}", uuid::Uuid::new_v4()),
            label: label.into(),
            server_url: server_url.into(),
            username: username.into(),
            use_files: true,
            use_calendar: true,
            use_mail: true,
            enabled: true,
            tint: 0,
            password: String::new(),
        }
    }
}

/// The Nextcloud account list, persisted at
/// `<config>/qos/nextcloud/accounts.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextcloudStore {
    #[serde(default = "default_version")]
    pub version: u32,
    pub accounts: Vec<NextcloudAccount>,
}

impl NextcloudStore {
    pub fn path() -> Option<PathBuf> {
        config_root().map(|r| r.join("nextcloud").join("accounts.json"))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| Self::from_json(&s))
            .unwrap_or_default()
    }

    /// # Errors
    ///
    /// Returns an error if no config directory can be resolved (`no config
    /// dir`), or if creating the parent directory or writing the JSON file
    /// fails (permissions, missing/read-only filesystem, etc).
    pub fn save(&self) -> std::io::Result<PathBuf> {
        let path = Self::path().ok_or_else(|| std::io::Error::other("no config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, self.to_json())?;
        Ok(path)
    }
}

// ---------------------------------------------------------------------------
// A read facade the panel renders over all four stores.
// ---------------------------------------------------------------------------

/// The protocol family of an account — drives grouping and the row icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccountKind {
    Mail,
    CalDav,
    WebDav,
    Nextcloud,
}

impl AccountKind {
    pub fn label(self) -> &'static str {
        match self {
            AccountKind::Mail => "Email",
            AccountKind::CalDav => "Calendar (CalDAV)",
            AccountKind::WebDav => "Files (WebDAV)",
            AccountKind::Nextcloud => "Nextcloud",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            AccountKind::Mail => "\u{f01ee}",      // md-email
            AccountKind::CalDav => "\u{f00ed}",    // md-calendar
            AccountKind::WebDav => "\u{f0256}",    // md-folder_outline
            AccountKind::Nextcloud => "\u{f015f}", // md-cloud
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            AccountKind::Mail => "mail",
            AccountKind::CalDav => "caldav",
            AccountKind::WebDav => "webdav",
            AccountKind::Nextcloud => "nextcloud",
        }
    }
}

/// A uniform, read-only view of one account for the panel. Built from any
/// of the four stores. This is render-only; edits go through the typed
/// store the account came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountView {
    pub id: String,
    pub kind: AccountKind,
    pub label: String,
    /// A one-line connection summary.
    pub summary: String,
    pub enabled: bool,
    pub tint: u8,
}

/// The full set of accounts the panel shows, loaded from all four stores.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountDirectory {
    pub mail: MailStore,
    pub calendar: DavStore,
    pub files: DavStore,
    pub nextcloud: NextcloudStore,
}

impl AccountDirectory {
    /// Load every store. Email and `CalDAV` accounts come from the shared
    /// canonical store ([`accounts::AccountStore`]) — the same store Mail and
    /// Calendar read, so the shapes can no longer drift apart. Legacy `WebDAV`
    /// and Nextcloud JSON is imported idempotently before projecting the view.
    pub fn load() -> Self {
        if accounts::config_root().is_some()
            && let Err(error) = recover_account_mutation()
        {
            tracing::warn!(target: "qos::settings", %error, "could not recover interrupted account mutation");
        }
        let mut shared = accounts::AccountStore::load();
        if let Some(root) = accounts::config_root() {
            let marker = accounts::accounts_dir_in(&root).join("legacy-settings-import-v1");
            if !marker.exists() {
                let changed = import_legacy_accounts(
                    &mut shared,
                    &DavStore::load_from(DavStore::files_path()),
                    &NextcloudStore::load(),
                );
                let persisted = if changed {
                    match shared.save() {
                        Ok(()) => true,
                        Err(error) => {
                            tracing::warn!(target: "qos::settings", %error, "could not persist imported legacy accounts");
                            false
                        }
                    }
                } else {
                    true
                };
                let mark_imported = || -> std::io::Result<()> {
                    if let Some(parent) = marker.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    atomic_file::write(&marker, b"imported\n")
                };
                if persisted && let Err(error) = mark_imported() {
                    // Without a durable marker the import safely repeats as
                    // upserts. Reporting this matters because a subsequently
                    // removed legacy account could otherwise reappear.
                    tracing::warn!(target: "qos::settings", %error, "could not mark legacy account import complete");
                }
            }
        }
        Self {
            mail: mail_store_from_shared(&shared),
            calendar: caldav_store_from_shared(&shared),
            files: webdav_store_from_shared(&shared),
            nextcloud: nextcloud_store_from_shared(&shared),
        }
    }

    /// Whether every store is empty — the panel uses this to substitute a
    /// populated sample so the surface is never blank.
    pub fn is_empty(&self) -> bool {
        self.mail.accounts.is_empty()
            && self.calendar.accounts.is_empty()
            && self.files.accounts.is_empty()
            && self.nextcloud.accounts.is_empty()
    }

    /// The uniform views for one kind, in store order.
    pub fn views_of(&self, kind: AccountKind) -> Vec<AccountView> {
        match kind {
            AccountKind::Mail => self
                .mail
                .accounts
                .iter()
                .map(|a| AccountView {
                    id: a.id.clone(),
                    kind,
                    label: a.label().to_string(),
                    summary: format!(
                        "{} · {}:{}",
                        a.config.email, a.config.imap_host, a.config.imap_port
                    ),
                    enabled: true,
                    tint: a.tint,
                })
                .collect(),
            AccountKind::CalDav => self.calendar.accounts.iter().map(dav_view_caldav).collect(),
            AccountKind::WebDav => self.files.accounts.iter().map(dav_view_webdav).collect(),
            AccountKind::Nextcloud => self.nextcloud.accounts.iter().map(nextcloud_view).collect(),
        }
    }
}

fn dav_view_caldav(a: &DavAccount) -> AccountView {
    AccountView {
        id: a.id.clone(),
        kind: AccountKind::CalDav,
        label: a.label.clone(),
        summary: format!("CalDAV · {}", short_url(&a.base_url)),
        enabled: a.enabled,
        tint: a.tint,
    }
}

fn dav_view_webdav(a: &DavAccount) -> AccountView {
    AccountView {
        id: a.id.clone(),
        kind: AccountKind::WebDav,
        label: a.label.clone(),
        summary: format!("WebDAV · {}", short_url(&a.base_url)),
        enabled: a.enabled,
        tint: a.tint,
    }
}

fn nextcloud_view(a: &NextcloudAccount) -> AccountView {
    let mut svc = Vec::new();
    if a.use_mail {
        svc.push("Mail");
    }
    if a.use_calendar {
        svc.push("Calendar");
    }
    if a.use_files {
        svc.push("Files");
    }
    AccountView {
        id: a.id.clone(),
        kind: AccountKind::Nextcloud,
        label: a.label.clone(),
        summary: format!("{} · {}", short_url(&a.server_url), svc.join(", ")),
        enabled: a.enabled,
        tint: a.tint,
    }
}

/// Trim a URL to host[/first-segment] for compact display.
pub fn short_url(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

fn default_true() -> bool {
    true
}
fn default_version() -> u32 {
    1
}

// ---------------------------------------------------------------------------
// Bridges onto the shared canonical store.
//
// Email and CalDAV accounts live in the one `accounts::AccountStore`. These
// project a filtered view of it back into the in-memory `MailStore` / `DavStore`
// shapes the pane already renders, so the pane code and its tests are unchanged
// while the on-disk store is now the shared one.
// ---------------------------------------------------------------------------

/// The shared-store type id for email accounts (Mail contributes these).
const MAIL_TYPE: &str = "email";
/// The shared-store type id for `CalDAV` accounts (Calendar contributes these).
const CALDAV_TYPE: &str = "caldav";
pub const WEBDAV_TYPE: &str = "webdav";
pub const NEXTCLOUD_TYPE: &str = "nextcloud";

/// Project the shared store's email accounts into a [`MailStore`].
fn mail_store_from_shared(shared: &accounts::AccountStore) -> MailStore {
    let ty = accounts::AccountTypeId::from(MAIL_TYPE);
    let accounts: Vec<MailAccount> = shared
        .list(&ty)
        .into_iter()
        .filter_map(|a| {
            // The opaque config deserialises into the same `AccountConfig`
            // shape; unknown fields (Mail's `auth`, the sidebar `collapsed`)
            // are ignored.
            let config: AccountConfig = serde_json::from_value(a.config.clone()).ok()?;
            Some(MailAccount {
                id: a.id.to_string(),
                config,
                collapsed: false,
                tint: a.tint,
            })
        })
        .collect();
    let active = shared
        .active_id(&ty)
        .and_then(|id| accounts.iter().position(|m| m.id == id.as_str()))
        .unwrap_or(0);
    MailStore { accounts, active }
}

/// Project the shared store's `CalDAV` accounts into a [`DavStore`] (the shape
/// the pane renders for the Calendar kind).
fn caldav_store_from_shared(shared: &accounts::AccountStore) -> DavStore {
    let ty = accounts::AccountTypeId::from(CALDAV_TYPE);
    let accounts = shared
        .list(&ty)
        .into_iter()
        .map(|a| DavAccount {
            id: a.id.to_string(),
            label: a.display_name.clone(),
            base_url: a
                .config
                .get("server_url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            username: a
                .config
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            enabled: a.enabled,
            tint: a.tint,
            password: String::new(),
        })
        .collect();
    DavStore {
        version: 1,
        accounts,
    }
}

fn webdav_store_from_shared(shared: &accounts::AccountStore) -> DavStore {
    let ty = accounts::AccountTypeId::from(WEBDAV_TYPE);
    DavStore {
        version: 1,
        accounts: shared
            .list(&ty)
            .into_iter()
            .filter_map(|a| {
                let mut config = a.config_as::<DavAccount>()?;
                config.id = a.id.to_string();
                config.label.clone_from(&a.display_name);
                config.enabled = a.enabled;
                config.tint = a.tint;
                config.password.clear();
                Some(config)
            })
            .collect(),
    }
}

fn nextcloud_store_from_shared(shared: &accounts::AccountStore) -> NextcloudStore {
    let ty = accounts::AccountTypeId::from(NEXTCLOUD_TYPE);
    NextcloudStore {
        version: 1,
        accounts: shared
            .list(&ty)
            .into_iter()
            .filter_map(|a| {
                let mut config = a.config_as::<NextcloudAccount>()?;
                config.id = a.id.to_string();
                config.label.clone_from(&a.display_name);
                config.enabled = a.enabled;
                config.tint = a.tint;
                config.password.clear();
                Some(config)
            })
            .collect(),
    }
}

/// Import old per-app account metadata without duplicating records. Credentials
/// were `serde(skip)` in those formats, so this operation cannot move or expose
/// a password; existing vault entries remain keyed by the same stable ids.
pub fn import_legacy_accounts(
    shared: &mut accounts::AccountStore,
    webdav: &DavStore,
    nextcloud: &NextcloudStore,
) -> bool {
    let before = shared.len();
    for legacy in &webdav.accounts {
        let id = accounts::AccountId::from(legacy.id.clone());
        if shared.get(&id).is_none() {
            let mut stored = accounts::Account::new(id, WEBDAV_TYPE);
            stored.display_name.clone_from(&legacy.label);
            stored.enabled = legacy.enabled;
            stored.tint = legacy.tint;
            stored.set_config(legacy);
            shared.upsert(stored);
        }
    }
    for legacy in &nextcloud.accounts {
        let id = accounts::AccountId::from(legacy.id.clone());
        if shared.get(&id).is_none() {
            let mut stored = accounts::Account::new(id, NEXTCLOUD_TYPE);
            stored.display_name.clone_from(&legacy.label);
            stored.enabled = legacy.enabled;
            stored.tint = legacy.tint;
            stored.set_config(legacy);
            shared.upsert(stored);
        }
    }
    shared.len() != before
}

/// The per-user qOS config root: `$XDG_CONFIG_HOME/qos` or `~/.config/qos`.
/// Shared by every account store and the region store so they all resolve
/// the same directory — and, through [`settings_store`], the same directory
/// the compositor and every other app resolve.
pub fn config_root() -> Option<PathBuf> {
    settings_store::config_root()
}

/// A deterministic, populated directory for `--shots` and tests so the
/// panel renders a realistic accounts list without touching disk.
pub fn sample_directory() -> AccountDirectory {
    let mut work = MailAccount::new(AccountConfig {
        email: "ada@example.com".into(),
        display_name: "Work — Fastmail".into(),
        imap_host: "imap.fastmail.com".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.fastmail.com".into(),
        smtp_port: 465,
        smtp_tls: true,
        username: "ada@example.com".into(),
        password: String::new(),
    });
    work.tint = 2;

    let mut personal = MailAccount::new(AccountConfig {
        email: "ada.personal@gmail.com".into(),
        display_name: "Personal — Gmail".into(),
        imap_host: "imap.gmail.com".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.gmail.com".into(),
        smtp_port: 465,
        smtp_tls: true,
        username: "ada.personal@gmail.com".into(),
        password: String::new(),
    });
    personal.tint = 5;

    let mut cal = DavAccount::new(
        "Team Calendar",
        "https://dav.example.com/calendars/ada/",
        "ada",
    );
    cal.tint = 3;

    let mut cloud = NextcloudAccount::new("Home Nextcloud", "https://cloud.example.com", "ada");
    cloud.use_mail = false;
    cloud.tint = 6;

    AccountDirectory {
        mail: MailStore {
            accounts: vec![work, personal],
            active: 0,
        },
        calendar: DavStore {
            version: 1,
            accounts: vec![cal],
        },
        files: DavStore::default(),
        nextcloud: NextcloudStore {
            version: 1,
            accounts: vec![cloud],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_store_round_trips_and_drops_password() {
        let mut acct = MailAccount::new(AccountConfig {
            email: "a@x.example".into(),
            password: "hunter2".into(),
            ..Default::default()
        });
        acct.tint = 4;
        let store = MailStore {
            accounts: vec![acct],
            active: 0,
        };
        let json = store.to_json();
        // The password must never reach the JSON.
        assert!(!json.contains("hunter2"));
        let back = MailStore::from_json(&json).expect("parse");
        // Everything except the (skipped) password round-trips.
        assert_eq!(back.accounts[0].config.email, "a@x.example");
        assert_eq!(back.accounts[0].tint, 4);
        assert_eq!(back.accounts[0].config.password, "");
    }

    #[test]
    fn mail_store_path_is_the_mail_contract_path() {
        // The relative path Mail reads.
        assert_eq!(MailStore::REL_PATH, "mail/accounts.json");
    }

    #[test]
    fn dav_and_nextcloud_round_trip() {
        let dav = DavStore {
            version: 1,
            accounts: vec![DavAccount::new("Cal", "https://d.example/c/", "ada")],
        };
        let back = DavStore::from_json(&dav.to_json()).unwrap();
        assert_eq!(dav, back);

        let nc = NextcloudStore {
            version: 1,
            accounts: vec![NextcloudAccount::new("NC", "https://cloud.example", "ada")],
        };
        let back = NextcloudStore::from_json(&nc.to_json()).unwrap();
        assert_eq!(nc, back);
    }

    #[test]
    fn dav_and_nextcloud_drop_passwords() {
        let mut a = DavAccount::new("Cal", "https://d/", "u");
        a.password = "secret".into();
        let store = DavStore {
            version: 1,
            accounts: vec![a],
        };
        assert!(!store.to_json().contains("secret"));

        let mut n = NextcloudAccount::new("NC", "https://c", "u");
        n.password = "nextsecret".into();
        let store = NextcloudStore {
            version: 1,
            accounts: vec![n],
        };
        assert!(!store.to_json().contains("nextsecret"));
    }

    #[test]
    fn directory_views_partition_by_kind() {
        let dir = sample_directory();
        assert_eq!(dir.views_of(AccountKind::Mail).len(), 2);
        assert_eq!(dir.views_of(AccountKind::CalDav).len(), 1);
        assert_eq!(dir.views_of(AccountKind::WebDav).len(), 0);
        assert_eq!(dir.views_of(AccountKind::Nextcloud).len(), 1);
    }

    #[test]
    fn mail_view_summary_includes_host() {
        let dir = sample_directory();
        let v = &dir.views_of(AccountKind::Mail)[0];
        assert!(v.summary.contains("imap.fastmail.com"));
    }

    #[test]
    fn nextcloud_view_lists_enabled_services() {
        let dir = sample_directory();
        let v = &dir.views_of(AccountKind::Nextcloud)[0];
        // mail disabled in the sample.
        assert!(v.summary.contains("Calendar"));
        assert!(v.summary.contains("Files"));
        assert!(!v.summary.contains("Mail"));
    }

    #[test]
    fn autofill_recognises_gmail_and_falls_back() {
        let mut c = AccountConfig {
            email: "x@gmail.com".into(),
            ..Default::default()
        };
        assert!(c.autofill_from_email());
        assert_eq!(c.imap_host, "imap.gmail.com");

        let mut c2 = AccountConfig {
            email: "x@acme.test".into(),
            ..Default::default()
        };
        assert!(!c2.autofill_from_email());
        assert_eq!(c2.imap_host, "imap.acme.test");
    }

    #[test]
    fn short_url_strips_scheme_and_trailing_slash() {
        assert_eq!(short_url("https://cloud.example.com/"), "cloud.example.com");
        assert_eq!(
            short_url("http://dav.example.com/calendars/ada/"),
            "dav.example.com/calendars/ada"
        );
    }

    #[test]
    fn empty_directory_is_detected() {
        assert!(AccountDirectory::default().is_empty());
        assert!(!sample_directory().is_empty());
    }

    #[test]
    fn legacy_webdav_and_nextcloud_import_is_idempotent_and_secret_free() {
        let mut web = DavAccount::new("Files", "https://dav.example/files", "ada");
        web.password = "must-not-migrate".into();
        let mut cloud = NextcloudAccount::new("Cloud", "https://cloud.example", "ada");
        cloud.password = "also-must-not-migrate".into();
        let files = DavStore {
            version: 1,
            accounts: vec![web.clone()],
        };
        let clouds = NextcloudStore {
            version: 1,
            accounts: vec![cloud.clone()],
        };
        let mut shared = accounts::AccountStore::new();

        assert!(import_legacy_accounts(&mut shared, &files, &clouds));
        assert!(!import_legacy_accounts(&mut shared, &files, &clouds));
        assert_eq!(shared.len(), 2);
        let json = serde_json::to_string(&shared).expect("canonical store serializes");
        assert!(!json.contains("must-not-migrate"));
        assert!(!json.contains("also-must-not-migrate"));
        assert_eq!(webdav_store_from_shared(&shared).accounts[0].id, web.id);
        assert_eq!(
            nextcloud_store_from_shared(&shared).accounts[0].id,
            cloud.id
        );
    }

    #[test]
    fn completed_legacy_import_does_not_resurrect_a_removed_account() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("legacy_account_no_resurrection");
        let legacy = DavAccount::new("Old files", "https://dav.example/files", "ada");
        DavStore {
            version: 1,
            accounts: vec![legacy.clone()],
        }
        .save_to(DavStore::files_path())
        .expect("seed legacy store");

        assert_eq!(AccountDirectory::load().files.accounts.len(), 1);
        let id = accounts::AccountId::from(legacy.id);
        let mut canonical = accounts::AccountStore::load();
        assert!(canonical.remove(&id));
        canonical.save().expect("persist removal");

        assert!(
            AccountDirectory::load().files.accounts.is_empty(),
            "the durable one-time marker prevents legacy metadata from resurrecting a removal"
        );
    }

    #[test]
    fn journal_recovers_each_interrupted_removal_phase() {
        for phase in 1..=3 {
            let _cfg = crate::prefs::test_env::ConfigGuard::new(&format!(
                "account_mutation_recovery_phase_{phase}"
            ));
            let id = accounts::AccountId::from(format!("recover-{phase}"));
            let mut original_store = accounts::AccountStore::new();
            let mut account = accounts::Account::new(id.clone(), NEXTCLOUD_TYPE);
            account.display_name = "Recover me".into();
            account.config = serde_json::json!({
                "id": id.as_str(), "label": "Recover me", "server_url": "https://cloud.example",
                "username": "ada", "use_files": true, "use_calendar": true, "use_mail": true
            });
            original_store.upsert(account);
            original_store.save().expect("seed metadata");
            let mut vault = accounts::SecretVault::open();
            vault.put(&id, &accounts::Secret::password("sealed-secret"));
            vault.try_save().expect("seed vault");
            let reporter = accounts::AccountSyncReporter::new(id.clone());
            reporter.succeeded().expect("seed status");
            let original_statuses = accounts::AccountSyncStatusStore::load();

            begin_account_mutation(&original_store, &original_statuses).expect("journal intent");
            let mut vault = accounts::SecretVault::open();
            vault.remove(&id);
            vault.try_save().expect("phase one");
            if phase >= 2 {
                let mut store = accounts::AccountStore::load();
                store.remove(&id);
                store.save().expect("phase two");
            }
            if phase >= 3 {
                let mut statuses = accounts::AccountSyncStatusStore::load();
                statuses.remove(&id);
                statuses.save().expect("phase three");
            }

            let recovered = AccountDirectory::load();
            assert_eq!(recovered.nextcloud.accounts.len(), 1, "phase {phase}");
            assert_eq!(
                accounts::SecretVault::open()
                    .get(&id)
                    .map(|secret| secret.password),
                Some("sealed-secret".into()),
                "phase {phase} restores encrypted credential"
            );
            assert!(
                accounts::AccountSyncStatusStore::load().get(&id).is_some(),
                "phase {phase} restores sync status"
            );
            assert!(!mutation_journal_path().expect("journal path").exists());
        }
    }
}
