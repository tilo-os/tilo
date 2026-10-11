//! The shared **external-accounts stores** — the on-disk contracts the
//! Settings "External Accounts" panel edits and the Mail / Calendar / file
//! apps consume.
//!
//! # Why Settings owns these
//!
//! Account inventory in Tilo is presented in one place: this app's External
//! Accounts panel. Password/app-password accounts for Mail, `CalDAV`, `WebDAV`,
//! and Nextcloud can be added, edited, tested, reauthenticated, and removed
//! there. Google Drive, `OneDrive` and Dropbox sign in through the browser
//! (`OAuth`), Nextcloud through its Login Flow v2, and `iCloud` Drive with an
//! Apple ID and verification code; `account_custody` keeps those grants and
//! hands the storage daemon only short-lived credentials. Mail's Gmail and
//! Outlook sign-in runs in Mail, through the same client registrations and
//! the same custody.
//!
//! # Where email + `CalDAV` live: the shared `accounts` crate
//!
//! Email and `CalDAV` accounts are persisted through the shared
//! [`accounts::AccountStore`] — one canonical JSON document at
//! `~/.config/tilo/accounts/store.json` — the **same** store Mail and Calendar
//! read. Settings owns the write lifecycle (add / edit / remove); those apps
//! consume it. Passwords are sealed in the accompanying encrypted
//! [`accounts::SecretVault`], never in the plaintext store.
//!
//! [`AccountDirectory::load`] projects that shared store into the in-memory
//! [`MailStore`] / [`DavStore`] shapes this pane renders, so the pane code is
//! unchanged while the on-disk store is the shared one. Consequently the
//! `MailStore` / `MailAccount` types below are now **render view-models**
//! populated from the shared store (around the shared
//! [`accounts::email::AccountConfig`]); every account kind writes the
//! canonical store.
//!
//! # Whose accounts these are
//!
//! In a host the accounts are the person's own record
//! ([`accounts::person::PersonAccounts`]), which follows them to every
//! device they sign in on and which Settings alone edits; the device's
//! `store.json` is kept its projection for the services that read it.
//! Outside a host (a bare app in a test) the file is the store.
//!
//! # Credentials are never serialised
//!
//! Passwords / tokens live in the OS secret store, not these JSON files.
//! Every credential field is `#[serde(skip)]`. Settings persists secrets
//! separately keyed by the account id; consumers ask the secret store for
//! them at connect time.

use std::path::PathBuf;

use accounts::email::AccountConfig;
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

/// Where the account stores, the credential vault and the mutation journal
/// live. The live root follows the signed-in user's config directory and the
/// session's credential custody; an explicit root pins every store under one
/// directory with a private local vault key, so an embedded or test instance
/// never reads, replays or deletes another instance's accounts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountRoot {
    explicit: Option<PathBuf>,
    /// The person's accounts, when a host opened their record.
    person: Option<PersonAccountsRecord>,
}

/// The person's record of their accounts, as [`AccountRoot`] keeps it.
#[derive(Clone)]
struct PersonAccountsRecord(ui::framework::PersonRecord<accounts::person::PersonAccounts>);

impl std::fmt::Debug for PersonAccountsRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PersonAccounts")
    }
}

/// A root is told apart by where its files live; the person has one
/// record of their accounts.
impl PartialEq for PersonAccountsRecord {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for PersonAccountsRecord {}

impl AccountRoot {
    /// The user's config root, resolved when each store is opened.
    pub fn live() -> Self {
        Self::default()
    }

    /// Every account store under `config_root` (the directory holding
    /// `accounts/`, `files/`, `nextcloud/` …).
    pub fn at(config_root: impl Into<PathBuf>) -> Self {
        Self {
            explicit: Some(config_root.into()),
            person: None,
        }
    }

    /// The same root, with the person's accounts kept in `record`.
    #[must_use]
    pub fn with_person(
        mut self,
        record: ui::framework::PersonRecord<accounts::person::PersonAccounts>,
    ) -> Self {
        self.person = Some(PersonAccountsRecord(record));
        self
    }

    /// The explicit config root, `None` for the live root.
    pub fn explicit(&self) -> Option<&std::path::Path> {
        self.explicit.as_deref()
    }

    /// Credential custody for cloud accounts over the same stores.
    pub fn custodian(&self) -> account_custody::Custodian {
        account_custody::Custodian::for_root(self.explicit.clone()).with_apple_icloud()
    }

    /// The config root, `None` when the live root has no usable home.
    pub fn path(&self) -> Option<PathBuf> {
        self.explicit.clone().or_else(accounts::config_root)
    }

    fn require(&self) -> Result<PathBuf, String> {
        self.path().ok_or_else(|| "no config dir".to_string())
    }

    fn require_io(&self) -> std::io::Result<PathBuf> {
        self.path()
            .ok_or_else(|| std::io::Error::other("no config dir"))
    }

    /// The person's accounts: their record in a host, else this device's
    /// file.
    pub fn store(&self) -> accounts::AccountStore {
        if let Some(PersonAccountsRecord(record)) = &self.person {
            return record.get().store();
        }
        self.path()
            .map(|root| accounts::AccountStore::load_in(&root))
            .unwrap_or_default()
    }

    /// Increases whenever the person's accounts change; `None` outside a
    /// host.
    pub fn revision(&self) -> Option<u64> {
        self.person
            .as_ref()
            .map(|PersonAccountsRecord(record)| record.revision())
    }

    /// Take into the person's accounts the accounts this device's file held
    /// before it became the record's projection: once, on the first open
    /// that keeps the record. After that the file only follows the record,
    /// so an account removed on another device is never brought back.
    /// Nothing to do outside a host.
    ///
    /// # Errors
    ///
    /// Returns an error when the record refuses an account.
    pub fn take_in_device_accounts(&self) -> std::io::Result<()> {
        let Some(PersonAccountsRecord(record)) = &self.person else {
            return Ok(());
        };
        let Some(root) = self.path() else {
            return Ok(());
        };
        let device = accounts::AccountStore::load_in(&root);
        for edit in record.get().taking_in(&device) {
            record
                .invoke(&edit)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
        Ok(())
    }

    /// Publish public metadata through Settings' existing personal record.
    ///
    /// # Errors
    /// Refuses a missing personal record or invalid public identity metadata.
    /// No fallback file or credential registry is created outside a host.
    pub fn publish_browser_identity(
        &self,
        root: String,
        origin: String,
        display_name: String,
    ) -> Result<(), String> {
        let Some(PersonAccountsRecord(record)) = &self.person else {
            return Err("Settings' personal account record is unavailable".into());
        };
        record
            .invoke(
                &accounts::person::PersonAccounts::PUBLISH_BROWSER_IDENTITY.with((
                    root,
                    origin,
                    display_name,
                )),
            )
            .map_err(|error| error.to_string())
    }

    /// Change browser picker visibility in the existing public account record.
    ///
    /// # Errors
    /// Refuses a missing record, a non-identity account or a refused edit.
    pub fn set_browser_identity_enabled(
        &self,
        account: String,
        enabled: bool,
    ) -> Result<(), String> {
        let Some(PersonAccountsRecord(record)) = &self.person else {
            return Err("Settings' personal account record is unavailable".into());
        };
        let current = record
            .get()
            .store()
            .get(&account.clone().into())
            .cloned()
            .ok_or("No such account")?;
        if current.type_id.as_str() != accounts::person::TILO_IDENTITY_TYPE {
            return Err("Not a browser identity account".into());
        }
        record
            .invoke(&accounts::person::PersonAccounts::SAVE_ACCOUNT.with((
                account,
                current.type_id.as_str().to_owned(),
                current.display_name,
                current.config,
                enabled,
                current.tint,
            )))
            .map_err(|error| error.to_string())
    }

    /// Remove public picker metadata; existing sign-in custody is untouched.
    ///
    /// # Errors
    /// Refuses a missing record, a non-identity account or a refused edit.
    pub fn remove_browser_identity(&self, account: String) -> Result<(), String> {
        let Some(PersonAccountsRecord(record)) = &self.person else {
            return Err("Settings' personal account record is unavailable".into());
        };
        let current = record
            .get()
            .store()
            .get(&account.clone().into())
            .cloned()
            .ok_or("No such account")?;
        if current.type_id.as_str() != accounts::person::TILO_IDENTITY_TYPE {
            return Err("Not a browser identity account".into());
        }
        record
            .invoke(
                &accounts::person::PersonAccounts::REMOVE_ACCOUNT
                    .with(ui::tilo_resource::Id::from_raw(account)),
            )
            .map_err(|error| error.to_string())
    }

    /// Keep this device's account file the projection of the person's
    /// accounts, for the services outside any app that read it. Nothing to
    /// do outside a host, where the file is the store.
    ///
    /// # Errors
    ///
    /// Returns an error when no config root resolves or the file cannot be
    /// written.
    pub fn project(&self) -> std::io::Result<()> {
        match &self.person {
            Some(PersonAccountsRecord(record)) => {
                record.get().project_in(&self.require_io()?).map(|_| ())
            }
            None => Ok(()),
        }
    }

    /// Make the person's accounts `store`: in a host, the edits that turn
    /// their record into it, then the device file its projection.
    ///
    /// # Errors
    ///
    /// Returns an error when the record refuses an edit, no config root
    /// resolves or the file cannot be written.
    pub fn save_store(&self, store: &accounts::AccountStore) -> std::io::Result<()> {
        let root = self.require_io()?;
        let Some(PersonAccountsRecord(record)) = &self.person else {
            return store.save_in(&root);
        };
        let refused =
            |error: ui::tilo_actions::ActionError| std::io::Error::other(error.to_string());
        let current = record.get().store();
        for account in current.all() {
            if store.get(&account.id).is_none() {
                record
                    .invoke(
                        &accounts::person::PersonAccounts::REMOVE_ACCOUNT
                            .with(ui::tilo_resource::Id::from_raw(account.id.0.clone())),
                    )
                    .map_err(refused)?;
            }
        }
        for account in store.all() {
            // The record keeps connection settings as an object; none is an
            // empty one.
            let mut account = account.clone();
            if account.config.is_null() {
                account.config = serde_json::json!({});
            }
            if current.get(&account.id) != Some(&account) {
                record
                    .invoke(&accounts::person::PersonAccounts::saving(&account))
                    .map_err(refused)?;
            }
        }
        let now = record.get().store();
        for account in store.all() {
            let wanted = store.active_id(&account.type_id);
            if wanted == Some(&account.id) && now.active_id(&account.type_id) != wanted {
                record
                    .invoke(
                        &accounts::person::PersonAccounts::SET_ACTIVE_ACCOUNT
                            .with(ui::tilo_resource::Id::from_raw(account.id.0.clone())),
                    )
                    .map_err(refused)?;
            }
        }
        record.get().project_in(&root).map(|_| ())
    }

    /// Operational sync health per account.
    pub fn statuses(&self) -> accounts::AccountSyncStatusStore {
        self.path()
            .map(|root| accounts::AccountSyncStatusStore::load_in(&root))
            .unwrap_or_default()
    }

    /// # Errors
    ///
    /// Returns an error when no config root resolves or the status store
    /// cannot be written.
    pub fn save_statuses(
        &self,
        statuses: &accounts::AccountSyncStatusStore,
    ) -> std::io::Result<()> {
        statuses.save_in(&self.require_io()?)
    }

    /// The encrypted credential vault.
    pub fn vault(&self) -> accounts::SecretVault {
        match &self.explicit {
            Some(root) => accounts::SecretVault::open_in(root),
            None => accounts::SecretVault::open(),
        }
    }

    /// The per-account sync health writer.
    pub fn sync_reporter(&self, id: accounts::AccountId) -> accounts::AccountSyncReporter {
        match &self.explicit {
            Some(root) => accounts::AccountSyncReporter::in_root(id, root.clone()),
            None => accounts::AccountSyncReporter::new(id),
        }
    }

    fn mutation_journal_path(&self) -> Result<PathBuf, String> {
        Ok(accounts::accounts_dir_in(&self.require()?).join("mutation-journal.json"))
    }

    /// Journal the pre-mutation stores and vault ciphertext so an
    /// interrupted metadata/vault/status mutation can be rolled back.
    ///
    /// # Errors
    ///
    /// Returns an error when the recovery check or durable journal write fails.
    pub fn begin_mutation(
        &self,
        previous_store: &accounts::AccountStore,
        previous_statuses: &accounts::AccountSyncStatusStore,
    ) -> Result<(), String> {
        self.recover_mutation()?;
        let root = self.require()?;
        let vault_ciphertext = std::fs::read(accounts::vault_dir_in(&root).join("vault.enc")).ok();
        let journal = AccountMutationJournal {
            store: previous_store.clone(),
            statuses: previous_statuses.clone(),
            vault_ciphertext,
        };
        let bytes = serde_json::to_vec(&journal)
            .map_err(|error| format!("could not serialize account transaction: {error}"))?;
        tilo_files::native::atomic::write_creating_parent_with_mode(
            &self.mutation_journal_path()?,
            &bytes,
            0o600,
        )
        .map_err(|error| format!("could not persist account transaction intent: {error}"))
    }

    /// # Errors
    ///
    /// Returns an error when the completed mutation journal cannot be removed.
    pub fn finish_mutation(&self) -> Result<(), String> {
        let path = self.mutation_journal_path()?;
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

    /// Roll back an interrupted metadata/vault/status mutation before
    /// presenting the account inventory. The journal contains ciphertext,
    /// never a plaintext password or token.
    ///
    /// # Errors
    ///
    /// Returns an error when the journal or the recovered stores cannot be read
    /// or durably restored.
    pub fn recover_mutation(&self) -> Result<(), String> {
        let path = self.mutation_journal_path()?;
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
        self.save_store(&journal.store)
            .map_err(|error| format!("could not recover account metadata: {error}"))?;
        self.save_statuses(&journal.statuses)
            .map_err(|error| format!("could not recover account sync status: {error}"))?;
        let vault_path = accounts::vault_dir_in(&self.require()?).join("vault.enc");
        match journal.vault_ciphertext {
            Some(ciphertext) => {
                tilo_files::native::atomic::write_creating_parent_with_mode(
                    &vault_path,
                    &ciphertext,
                    0o600,
                )
                .map_err(|error| format!("could not recover credential vault: {error}"))?;
            }
            None => match std::fs::remove_file(&vault_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("could not recover credential vault: {error}")),
            },
        }
        self.finish_mutation()
    }

    /// Recover an interrupted mutation, logging rather than failing: the
    /// inventory is still worth presenting.
    pub(crate) fn recover_mutation_or_warn(&self) {
        if self.path().is_some()
            && let Err(error) = self.recover_mutation()
        {
            tracing::warn!(target: "tilo::settings", %error, "could not recover interrupted account mutation");
        }
    }
}

// ---------------------------------------------------------------------------
// Mail view-model.
// ---------------------------------------------------------------------------

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

/// The mail account list — Mail's `AccountStore` shape.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailStore {
    pub accounts: Vec<MailAccount>,
    #[serde(default)]
    pub active: usize,
}

impl MailStore {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.accounts.len();
        self.accounts.retain(|a| a.id != id);
        self.accounts.len() != before
    }
}

// ---------------------------------------------------------------------------
// DAV store — CalDAV (calendar) and WebDAV (files).
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

/// A DAV account list. The *same* struct backs both the calendar and the
/// files view of the canonical store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DavStore {
    #[serde(default = "default_version")]
    pub version: u32,
    pub accounts: Vec<DavAccount>,
}

impl DavStore {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
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

/// The Nextcloud account list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextcloudStore {
    #[serde(default = "default_version")]
    pub version: u32,
    pub accounts: Vec<NextcloudAccount>,
}

impl NextcloudStore {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
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
    GoogleDrive,
    OneDrive,
    Dropbox,
    ICloudDrive,
}

impl AccountKind {
    /// Every kind, in panel order.
    pub const ALL: [Self; 8] = [
        Self::Mail,
        Self::CalDav,
        Self::WebDav,
        Self::Nextcloud,
        Self::GoogleDrive,
        Self::OneDrive,
        Self::Dropbox,
        Self::ICloudDrive,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AccountKind::Mail => "Email",
            AccountKind::CalDav => "Calendar",
            AccountKind::WebDav => "Files",
            AccountKind::Nextcloud => "Nextcloud",
            AccountKind::GoogleDrive => "Google Drive",
            AccountKind::OneDrive => "OneDrive",
            AccountKind::Dropbox => "Dropbox",
            AccountKind::ICloudDrive => "iCloud Drive",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            AccountKind::Mail => "\u{f01ee}",   // md-email
            AccountKind::CalDav => "\u{f00ed}", // md-calendar
            AccountKind::WebDav => "\u{f0256}", // md-folder_outline
            AccountKind::Nextcloud
            | AccountKind::GoogleDrive
            | AccountKind::OneDrive
            | AccountKind::Dropbox
            | AccountKind::ICloudDrive => "\u{f015f}", // md-cloud
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            AccountKind::Mail => "mail",
            AccountKind::CalDav => "caldav",
            AccountKind::WebDav => "webdav",
            AccountKind::Nextcloud => "nextcloud",
            AccountKind::GoogleDrive => "google-drive",
            AccountKind::OneDrive => "onedrive",
            AccountKind::Dropbox => "dropbox",
            AccountKind::ICloudDrive => "icloud-drive",
        }
    }

    /// The canonical store's type id for accounts of this kind.
    pub fn type_id(self) -> &'static str {
        match self {
            AccountKind::Mail => MAIL_TYPE,
            AccountKind::CalDav => CALDAV_TYPE,
            AccountKind::WebDav => WEBDAV_TYPE,
            AccountKind::Nextcloud => NEXTCLOUD_TYPE,
            AccountKind::GoogleDrive => account_custody::providers::GOOGLE_DRIVE_TYPE,
            AccountKind::OneDrive => account_custody::providers::ONEDRIVE_TYPE,
            AccountKind::Dropbox => account_custody::providers::DROPBOX_TYPE,
            AccountKind::ICloudDrive => account_custody::providers::ICLOUD_DRIVE_TYPE,
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.slug() == slug)
    }

    pub fn from_type_id(type_id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.type_id() == type_id)
    }

    /// The `OAuth` provider behind a cloud storage kind.
    pub fn oauth_cloud(self) -> Option<account_custody::OAuthCloud> {
        account_custody::OAuthCloud::from_type_id(self.type_id())
    }

    /// Google Drive, `OneDrive`, Dropbox and `iCloud` Drive: accounts held
    /// entirely by credential custody, listed from the canonical store.
    pub fn is_cloud_storage(self) -> bool {
        self.oauth_cloud().is_some() || self == AccountKind::ICloudDrive
    }

    /// Whether accounts of this kind are connected through a sign-in flow
    /// (browser or Apple ID) rather than a typed password.
    pub fn signs_in(self) -> bool {
        self.is_cloud_storage() || self == AccountKind::Nextcloud
    }

    /// Whether the provider is reached through an undocumented API and is
    /// supported on a best-effort basis.
    pub fn best_effort(self) -> bool {
        self == AccountKind::ICloudDrive
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
    /// Google Drive, `OneDrive`, Dropbox and `iCloud` Drive accounts,
    /// straight from the canonical store.
    pub cloud: Vec<accounts::Account>,
}

impl AccountDirectory {
    /// Load every store. Email and `CalDAV` accounts come from the shared
    /// canonical store ([`accounts::AccountStore`]) — the same store Mail and
    /// Calendar read, so the shapes can no longer drift apart.
    pub fn load() -> Self {
        Self::load_in(&AccountRoot::live())
    }

    /// [`Self::load`] from the stores under `root`.
    pub fn load_in(root: &AccountRoot) -> Self {
        root.recover_mutation_or_warn();
        let shared = root.store();
        Self {
            mail: mail_store_from_shared(&shared),
            calendar: caldav_store_from_shared(&shared),
            files: webdav_store_from_shared(&shared),
            nextcloud: nextcloud_store_from_shared(&shared),
            cloud: shared
                .all()
                .iter()
                .filter(|account| {
                    AccountKind::from_type_id(account.type_id.as_str())
                        .is_some_and(AccountKind::is_cloud_storage)
                })
                .cloned()
                .collect(),
        }
    }

    /// Whether every store is empty — the panel uses this to substitute a
    /// populated sample so the surface is never blank.
    pub fn is_empty(&self) -> bool {
        self.mail.accounts.is_empty()
            && self.calendar.accounts.is_empty()
            && self.files.accounts.is_empty()
            && self.nextcloud.accounts.is_empty()
            && self.cloud.is_empty()
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
                    summary: a.config.email.clone(),
                    enabled: true,
                    tint: a.tint,
                })
                .collect(),
            AccountKind::CalDav => self.calendar.accounts.iter().map(dav_view_caldav).collect(),
            AccountKind::WebDav => self.files.accounts.iter().map(dav_view_webdav).collect(),
            AccountKind::Nextcloud => self.nextcloud.accounts.iter().map(nextcloud_view).collect(),
            AccountKind::GoogleDrive
            | AccountKind::OneDrive
            | AccountKind::Dropbox
            | AccountKind::ICloudDrive => self
                .cloud
                .iter()
                .filter(|account| account.type_id.as_str() == kind.type_id())
                .map(|account| cloud_view(kind, account))
                .collect(),
        }
    }
}

fn dav_view_caldav(a: &DavAccount) -> AccountView {
    AccountView {
        id: a.id.clone(),
        kind: AccountKind::CalDav,
        label: a.label.clone(),
        summary: short_url(&a.base_url),
        enabled: a.enabled,
        tint: a.tint,
    }
}

fn dav_view_webdav(a: &DavAccount) -> AccountView {
    AccountView {
        id: a.id.clone(),
        kind: AccountKind::WebDav,
        label: a.label.clone(),
        summary: short_url(&a.base_url),
        enabled: a.enabled,
        tint: a.tint,
    }
}

fn nextcloud_view(a: &NextcloudAccount) -> AccountView {
    AccountView {
        id: a.id.clone(),
        kind: AccountKind::Nextcloud,
        label: a.label.clone(),
        summary: short_url(&a.server_url),
        enabled: a.enabled,
        tint: a.tint,
    }
}

fn cloud_view(kind: AccountKind, account: &accounts::Account) -> AccountView {
    let identity = account
        .config
        .get("identity")
        .and_then(serde_json::Value::as_str)
        .filter(|identity| !identity.is_empty());
    let summary = match (identity, kind.oauth_cloud()) {
        (Some(identity), _) => identity.to_string(),
        (None, Some(cloud)) => format!("Signed in with {}", cloud.sign_in_with()),
        (None, None) => "Apple ID".to_string(),
    };
    AccountView {
        id: account.id.to_string(),
        kind,
        label: if account.display_name.is_empty() {
            kind.label().to_string()
        } else {
            account.display_name.clone()
        },
        summary,
        enabled: account.enabled,
        tint: account.tint,
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
            // The opaque config is Mail's own `AccountConfig`; unknown
            // fields (the sidebar `collapsed`) are ignored.
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

/// The per-user Tilo config root: `$XDG_CONFIG_HOME/tilo` or `~/.config/tilo`.
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
        display_name: "Work email".into(),
        imap_host: "imap.fastmail.com".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.fastmail.com".into(),
        smtp_port: 465,
        smtp_tls: true,
        username: "ada@example.com".into(),
        ..Default::default()
    });
    work.tint = 2;

    let mut personal = MailAccount::new(AccountConfig {
        email: "ada.personal@gmail.com".into(),
        display_name: "Personal email".into(),
        imap_host: "imap.gmail.com".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.gmail.com".into(),
        smtp_port: 465,
        smtp_tls: true,
        username: "ada.personal@gmail.com".into(),
        ..Default::default()
    });
    personal.tint = 5;

    let mut cal = DavAccount::new(
        "Team calendar",
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
        cloud: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_profiles_do_not_take_in_another_profiles_browser_identity() {
        use ui::framework::testing::TestApp;

        let mut before = TestApp::<crate::resource::SettingsShell>::new().unwrap();
        let key = "ef".repeat(32);
        let origin = "https://isolated-profile.example";
        let id = format!("settings.browser-identity:tilo-identity:{key}@{origin}");
        assert!(
            before
                .actions()
                .unwrap()
                .iter()
                .all(|action| action.address.resource != id)
        );

        let mut publisher = TestApp::<crate::resource::SettingsShell>::for_person(&key).unwrap();
        let record = publisher
            .person::<accounts::person::PersonAccounts>()
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        AccountRoot::at(directory.path())
            .with_person(record)
            .publish_browser_identity(key, origin.into(), "Private profile".into())
            .unwrap();
        publisher.settle();
        assert!(
            publisher
                .actions()
                .unwrap()
                .iter()
                .any(|action| action.address.resource == id)
        );

        let mut after = TestApp::<crate::resource::SettingsShell>::new().unwrap();
        assert!(
            after
                .actions()
                .unwrap()
                .iter()
                .all(|action| action.address.resource != id),
            "a fresh profile must not import another profile's browser identity through the session account file"
        );
        assert!(
            before
                .actions()
                .unwrap()
                .iter()
                .all(|action| action.address.resource != id)
        );
    }

    #[test]
    fn browser_identity_publication_is_a_settings_owned_personal_record_action() {
        use accounts::person::PersonAccounts;
        let root_key = "ab".repeat(32);
        let mut app =
            ui::framework::testing::TestApp::<crate::resource::SettingsShell>::for_person(
                &root_key,
            )
            .unwrap();
        let record = app.person::<PersonAccounts>().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = AccountRoot::at(dir.path()).with_person(record.clone());
        root.publish_browser_identity(
            root_key.clone(),
            "https://host.example".into(),
            "Work".into(),
        )
        .unwrap();
        assert_eq!(
            record
                .get()
                .browser_accounts(&root_key, "https://host.example")
                .len(),
            1
        );
        assert!(
            record
                .get()
                .browser_accounts(&"cd".repeat(32), "https://host.example")
                .is_empty()
        );
        let choice = record
            .get()
            .browser_accounts(&root_key, "https://host.example")[0]
            .id
            .clone();
        let actions = app.actions().unwrap();
        assert!(actions.iter().any(|action| action.address.resource
            == format!("settings.browser-identity:{choice}")
            && action.address.action == "set-browser-identity-enabled"));
        app.call_on(
            &format!("settings.browser-identity:{choice}"),
            "set-browser-identity-enabled",
            serde_json::json!({}),
        )
        .unwrap();
        assert!(
            record
                .get()
                .browser_accounts(&root_key, "https://host.example")
                .is_empty()
        );
        // Republishing is idempotent and never silently restores a hidden choice.
        root.publish_browser_identity(
            root_key.clone(),
            "https://host.example".into(),
            "Another name".into(),
        )
        .unwrap();
        assert!(
            record
                .get()
                .browser_accounts(&root_key, "https://host.example")
                .is_empty()
        );
        app.call_on(
            &format!("settings.browser-identity:{choice}"),
            "set-browser-identity-enabled",
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(
            record
                .get()
                .browser_accounts(&root_key, "https://host.example")
                .len(),
            1
        );
        app.call_confirmed_on(
            &format!("settings.browser-identity:{choice}"),
            "remove-browser-identity",
            serde_json::json!({}),
        )
        .unwrap();
        assert!(
            record
                .get()
                .browser_accounts(&root_key, "https://host.example")
                .is_empty()
        );
        // Public metadata never provisions this device's credential vault.
        assert!(!dir.path().join("accounts").exists());
        assert!(
            AccountRoot::at(dir.path())
                .publish_browser_identity(root_key, "https://host.example".into(), "Work".into())
                .is_err()
        );
    }

    /// In a host the accounts are the person's record: saving edits it,
    /// reading reads it, and this device's file follows as its projection.
    #[test]
    fn in_a_host_the_accounts_are_the_persons_record_and_the_file_follows() {
        use accounts::person::PersonAccounts;
        let mut app = ui::framework::testing::TestApp::<crate::resource::SettingsShell>::new()
            .expect("settings opens headlessly");
        let record = app
            .person::<PersonAccounts>()
            .expect("Settings keeps the person's accounts");
        assert!(record.editable(), "Settings edits the person's accounts");
        let dir = tempfile::tempdir().unwrap();
        let root = AccountRoot::at(dir.path()).with_person(record.clone());

        let mut account = accounts::Account::new("a@x.example", "email");
        account.display_name = "Work".into();
        account.config = serde_json::json!({ "imap_host": "imap.x.example" });
        let mut store = accounts::AccountStore::new();
        store.upsert(account.clone());
        root.save_store(&store).unwrap();
        assert_eq!(record.get().store().get(&account.id), Some(&account));
        assert_eq!(root.store().get(&account.id), Some(&account));
        assert_eq!(
            accounts::AccountStore::load_in(dir.path()).get(&account.id),
            Some(&account),
            "the device file is the record's projection"
        );

        store.remove(&account.id);
        root.save_store(&store).unwrap();
        assert!(record.get().store().is_empty());
        assert!(accounts::AccountStore::load_in(dir.path()).is_empty());
    }

    /// The accounts this device's file held before it followed the person's
    /// record are taken in once, the record's own accounts stay, and a file
    /// that is the record's projection brings nothing back.
    #[test]
    fn the_device_files_accounts_are_taken_into_the_persons_record_once() {
        use accounts::person::PersonAccounts;
        let mut app = ui::framework::testing::TestApp::<crate::resource::SettingsShell>::new()
            .expect("settings opens headlessly");
        let record = app.person::<PersonAccounts>().expect("person accounts");
        let dir = tempfile::tempdir().unwrap();
        let root = AccountRoot::at(dir.path()).with_person(record.clone());
        let mut kept = accounts::Account::new("kept@x.example", "email");
        kept.config = serde_json::json!({ "imap": "imap.x.example" });
        record.invoke(&PersonAccounts::saving(&kept)).unwrap();

        // An account without connection settings is kept with empty ones.
        let mut local = accounts::Account::new("cal-1", "caldav");
        let mut device = accounts::AccountStore::new();
        device.upsert(local.clone());
        device.save_in(dir.path()).unwrap();
        local.config = serde_json::json!({});

        root.take_in_device_accounts()
            .and_then(|()| root.project())
            .unwrap();
        let person = record.get().store();
        assert_eq!(person.get(&local.id), Some(&local));
        assert_eq!(person.get(&kept.id), Some(&kept));
        assert_eq!(accounts::AccountStore::load_in(dir.path()), person);

        // Removed elsewhere: the projected file does not bring it back.
        record
            .invoke(&PersonAccounts::REMOVE_ACCOUNT.with(ui::tilo_resource::Id::from_raw("cal-1")))
            .unwrap();
        root.take_in_device_accounts().unwrap();
        assert!(record.get().store().get(&local.id).is_none());
    }

    /// Every account store round-trips through JSON at its contract path, and
    /// no store ever writes a password.
    #[test]
    fn account_stores_round_trip_without_passwords() {
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
        assert!(
            !json.contains("hunter2"),
            "mail password must never reach the JSON"
        );
        let back = MailStore::from_json(&json).expect("parse");
        // Everything except the (skipped) password round-trips.
        assert_eq!(back.accounts[0].config.email, "a@x.example");
        assert_eq!(back.accounts[0].tint, 4);
        assert_eq!(back.accounts[0].config.password, "");

        let dav = DavStore {
            version: 1,
            accounts: vec![DavAccount::new("Cal", "https://d.example/c/", "ada")],
        };
        assert_eq!(DavStore::from_json(&dav.to_json()).unwrap(), dav);
        let nc = NextcloudStore {
            version: 1,
            accounts: vec![NextcloudAccount::new("NC", "https://cloud.example", "ada")],
        };
        assert_eq!(NextcloudStore::from_json(&nc.to_json()).unwrap(), nc);

        let mut a = DavAccount::new("Cal", "https://d/", "u");
        a.password = "secret".into();
        let store = DavStore {
            version: 1,
            accounts: vec![a],
        };
        assert!(!store.to_json().contains("secret"), "dav password leaked");
        let mut n = NextcloudAccount::new("NC", "https://c", "u");
        n.password = "nextsecret".into();
        let store = NextcloudStore {
            version: 1,
            accounts: vec![n],
        };
        assert!(
            !store.to_json().contains("nextsecret"),
            "nextcloud password leaked"
        );
    }

    #[test]
    fn account_directory_views_summarise_and_autofill() {
        let dir = sample_directory();
        assert!(!dir.is_empty());
        assert!(AccountDirectory::default().is_empty());
        for (kind, want) in [
            (AccountKind::Mail, 2),
            (AccountKind::CalDav, 1),
            (AccountKind::WebDav, 0),
            (AccountKind::Nextcloud, 1),
        ] {
            assert_eq!(dir.views_of(kind).len(), want, "{kind:?} views");
        }
        let mail = &dir.views_of(AccountKind::Mail)[0];
        assert_eq!(mail.summary, "ada@example.com");
        assert_eq!(dir.mail.accounts[0].config.imap_host, "imap.fastmail.com");
        let cloud = &dir.views_of(AccountKind::Nextcloud)[0];
        assert_eq!(cloud.summary, "cloud.example.com");

        for (url, short) in [
            ("https://cloud.example.com/", "cloud.example.com"),
            (
                "http://dav.example.com/calendars/ada/",
                "dav.example.com/calendars/ada",
            ),
        ] {
            assert_eq!(short_url(url), short, "{url}");
        }
    }

    #[test]
    fn journal_recovers_each_interrupted_removal_phase() {
        for phase in 1..=3 {
            let dir = tempfile::tempdir().expect("scratch config root");
            let root = AccountRoot::at(dir.path().join("tilo"));
            let id = accounts::AccountId::from(format!("recover-{phase}"));
            let mut original_store = accounts::AccountStore::new();
            let mut account = accounts::Account::new(id.clone(), NEXTCLOUD_TYPE);
            account.display_name = "Recover me".into();
            account.config = serde_json::json!({
                "id": id.as_str(), "label": "Recover me", "server_url": "https://cloud.example",
                "username": "ada", "use_files": true, "use_calendar": true, "use_mail": true
            });
            original_store.upsert(account);
            root.save_store(&original_store).expect("seed metadata");
            let mut vault = root.vault();
            vault.put(&id, &accounts::Secret::password("sealed-secret"));
            vault.try_save().expect("seed vault");
            root.sync_reporter(id.clone())
                .succeeded()
                .expect("seed status");
            let original_statuses = root.statuses();

            root.begin_mutation(&original_store, &original_statuses)
                .expect("journal intent");
            let mut vault = root.vault();
            vault.remove(&id);
            vault.try_save().expect("phase one");
            if phase >= 2 {
                let mut store = root.store();
                store.remove(&id);
                root.save_store(&store).expect("phase two");
            }
            if phase >= 3 {
                let mut statuses = root.statuses();
                statuses.remove(&id);
                root.save_statuses(&statuses).expect("phase three");
            }

            let recovered = AccountDirectory::load_in(&root);
            assert_eq!(recovered.nextcloud.accounts.len(), 1, "phase {phase}");
            assert_eq!(
                root.vault().get(&id).map(|secret| secret.password),
                Some("sealed-secret".into()),
                "phase {phase} restores encrypted credential"
            );
            assert!(
                root.statuses().get(&id).is_some(),
                "phase {phase} restores sync status"
            );
            assert!(!root.mutation_journal_path().expect("journal path").exists());
        }
    }
}
