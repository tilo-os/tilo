//! The workspace's accounts as one record: the mail, calendar and messaging
//! accounts configured in it, which every member and device of the
//! workspace reads as the workspace's grants allow. Their secrets are never
//! in it.

use std::path::{Path, PathBuf};

use tilo_resource::{
    AppNamed, Collection, Id, Invoke, Model, Refusal, Replica, SharedResource, resource,
};

use crate::{Account, AccountId, AccountTypeId};

/// The app that keeps the workspace's accounts.
pub use tilo_app_id::apps::SETTINGS as ACCOUNTS_OWNER;

/// A Tilo identity's public connection metadata, edited in shared accounts.
pub const TILO_IDENTITY_TYPE: &str = "tilo.identity";

/// Public fields needed to choose an existing browser identity. This is a
/// projection of Settings' account record, not a credential or admission grant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserAccount {
    /// The shared account's stable id.
    pub id: String,
    /// The chosen account name.
    pub display_name: String,
    /// Public root key matching the profile that holds the account record.
    pub user_public_key: String,
}

/// One account of the workspace's, without its secret.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ListedAccount {
    /// The kind of account (`"email"`, `"caldav"`, …).
    pub type_id: String,
    /// The name shown for it ("Work — Fastmail").
    pub display_name: String,
    /// Type-specific connection settings. Never a secret. Each account type
    /// shapes its own; the record keeps them as the JSON they are, a map of
    /// settings, so two devices changing different settings both keep theirs.
    pub config: serde_json::Value,
    /// Whether it is in use.
    pub enabled: bool,
    /// Tints the account's avatar so several read as distinct.
    pub tint: u8,
    /// How its last sync went, as the device that last synced it reported.
    pub sync: Option<AccountSync>,
}

/// How an account's sync is going, as the device that reported last saw it.
/// Several devices may sync one account; the last report wins.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct AccountSync {
    /// The device that reported it, by its key in hex.
    pub device: String,
    pub health: SyncHealth,
    /// When that device last tried, in milliseconds since the Unix epoch.
    pub attempted_ms: u64,
    /// When a sync last went through.
    pub succeeded_ms: Option<u64>,
    /// When its person last signed in, while nothing has synced since: a
    /// sign-in proves the account works before its first sync does.
    pub signed_in_ms: Option<u64>,
    /// What a person reads about it: a short summary, never a server's own
    /// words, which can carry a credential.
    pub message: String,
}

pub use crate::SyncHealth;

/// The longest message a sync report keeps.
pub const SYNC_MESSAGE_LIMIT: usize = 160;

/// How much newer a success must be than the one kept before it is written
/// again while nothing else changed: a device syncing every minute does not
/// write every minute.
pub const SYNC_SUCCESS_REFRESH_MS: u64 = 10 * 60 * 1000;

/// Every account the workspace has, without their secrets. Which one a member's
/// window shows is that member's own preference, kept beside their Mail settings.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct AccountList {
    /// Keyed by the account's id, the id its secret is kept under.
    pub accounts: Collection<ListedAccount>,
}

impl AccountList {
    /// The kind its record is of, for a crate that names the record without the resource model.
    #[must_use]
    pub fn kind() -> &'static str {
        <Self as tilo_resource::Resource>::KIND
    }
}

#[resource(
    kind = "tilo.accounts",
    read_content = tilo_resource::read::fields
)]
impl AccountList {
    /// Project public browser choices from the existing typed account document.
    pub fn browser_accounts_from_doc(
        doc: &tilo_resource::Doc,
        root: &str,
        origin: &str,
    ) -> Result<Vec<BrowserAccount>, tilo_resource::ModelError> {
        use tilo_resource::Resource as _;
        Ok(Self::read(doc)?.browser_accounts(root, origin))
    }

    /// Existing enabled identity accounts for this profile and exact host.
    #[must_use]
    pub fn browser_accounts(&self, root: &str, origin: &str) -> Vec<BrowserAccount> {
        if root.len() != 64 || !root.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Vec::new();
        }
        let Ok(url) = url::Url::parse(origin) else {
            return Vec::new();
        };
        if url.scheme() != "https" || url.origin().ascii_serialization() != origin {
            return Vec::new();
        }
        self.accounts
            .iter()
            .filter(|(_, account)| {
                account.enabled
                    && account.type_id == TILO_IDENTITY_TYPE
                    && account
                        .config
                        .get("userPublicKey")
                        .and_then(serde_json::Value::as_str)
                        == Some(root)
                    && account
                        .config
                        .get("origin")
                        .and_then(serde_json::Value::as_str)
                        == Some(origin)
            })
            .take(64)
            .map(|(id, account)| BrowserAccount {
                id: id.as_str().to_owned(),
                display_name: account.display_name.chars().take(160).collect(),
                user_public_key: root.to_owned(),
            })
            .collect()
    }

    /// Publish this signed-in identity's public browser connection metadata.
    #[action]
    pub fn publish_browser_identity(
        &mut self,
        root: String,
        origin: String,
        display_name: String,
    ) -> Result<(), Refusal> {
        if root.len() != 64
            || !root.bytes().all(|byte| byte.is_ascii_hexdigit())
            || root != root.to_ascii_lowercase()
            || display_name.trim().is_empty()
            || display_name.chars().count() > 160
            || display_name.chars().any(char::is_control)
        {
            return Err(Refusal::from(
                "a bounded public identity and name are required",
            ));
        }
        let url =
            url::Url::parse(&origin).map_err(|_| Refusal::from("an HTTPS origin is required"))?;
        if url.scheme() != "https"
            || url.origin().ascii_serialization() != origin
            || origin.len() > 2048
        {
            return Err(Refusal::from("a canonical HTTPS origin is required"));
        }
        let id = format!("tilo-identity:{root}@{origin}");
        let key = Id::from_raw(id.clone());
        if let Some(existing) = self.accounts.get(&key) {
            if existing.type_id != TILO_IDENTITY_TYPE {
                return Err(Refusal::from("the identity account id is already in use"));
            }
            // No overwrite: removal/disable/name edits are ordinary Settings
            // actions. A normal sign-in must not silently reverse them.
            return Ok(());
        }
        self.keep_account(
            id,
            TILO_IDENTITY_TYPE.into(),
            display_name,
            serde_json::json!({"userPublicKey": root, "origin": origin}),
            true,
            0,
        )
    }

    /// Save an account: add it, or replace the one with its id.
    ///
    /// Whoever the workspace's policy lets change its accounts, a person or
    /// an agent acting for one, may save an account or remove it.
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
        self.keep_account(id, type_id, display_name, config, enabled, tint)
    }

    /// Keep an account as [`Self::save_account`] does once it may: for code
    /// of this device that fills the workspace's accounts itself.
    ///
    /// # Errors
    /// The account names no id or no kind.
    pub fn keep_account(
        &mut self,
        id: String,
        type_id: String,
        display_name: String,
        config: serde_json::Value,
        enabled: bool,
        tint: u8,
    ) -> Result<(), Refusal> {
        if id.trim().is_empty() || type_id.trim().is_empty() {
            return Err(Refusal::from("an account needs an id and a type"));
        }
        let key = Id::from_raw(id);
        let mut account = ListedAccount {
            type_id,
            display_name,
            config,
            enabled,
            tint,
            sync: None,
        };
        if let Some(existing) = self.accounts.get_mut(&key) {
            // Saving what an account is keeps how its sync went.
            account.sync = existing.sync.take();
            *existing = account;
        } else {
            self.accounts.push(key, account);
        }
        Ok(())
    }

    /// Keep `account` as [`Self::keep_account`] does.
    ///
    /// # Errors
    /// The account names no id or no kind.
    pub fn keep(&mut self, account: &Account) -> Result<(), Refusal> {
        self.keep_account(
            account.id.to_string(),
            account.type_id.to_string(),
            account.display_name.clone(),
            account.config.clone(),
            account.enabled,
            account.tint,
        )
    }

    /// Report how an account's sync went on the device `device`: what every
    /// member's window says about it. The last report wins. A report that
    /// says what the account already says is not written, unless it is a
    /// success more than ten minutes newer than the one kept.
    ///
    /// Whoever may save an account may report its sync.
    #[action]
    pub fn report_sync(
        &mut self,
        #[param("The account's id")] account: Id,
        #[param("The device that synced it, by its key in hex")] device: String,
        #[param("Where its sync stands")] health: SyncHealth,
        #[param("When, in milliseconds since the Unix epoch")] at_ms: u64,
        #[param("Whether its person just signed in to it")] signed_in: bool,
        #[param("A short summary a person reads, never a server's own words")] message: String,
    ) -> Result<(), Refusal> {
        let listed = self
            .accounts
            .get_mut(&account)
            .ok_or_else(|| Refusal::from("there is no such account"))?;
        let previous = listed.sync.clone().unwrap_or_default();
        let message = sync_message(&message);
        let succeeded = health == SyncHealth::Healthy;
        let next = AccountSync {
            device,
            health,
            attempted_ms: at_ms,
            succeeded_ms: if succeeded {
                Some(at_ms)
            } else {
                previous.succeeded_ms
            },
            // A sign-in stands until the next sync says how it went.
            signed_in_ms: signed_in.then_some(at_ms),
            message,
        };
        let unchanged = listed.sync.is_some()
            && previous.health == next.health
            && previous.message == next.message
            && previous.signed_in_ms.is_some() == next.signed_in_ms.is_some()
            && !(succeeded
                && at_ms
                    >= previous
                        .succeeded_ms
                        .unwrap_or_default()
                        .saturating_add(SYNC_SUCCESS_REFRESH_MS));
        if !unchanged {
            listed.sync = Some(next);
        }
        Ok(())
    }

    /// Remove account.
    #[action]
    pub fn remove_account(&mut self, account: Id) -> Result<(), Refusal> {
        self.accounts
            .remove(&account)
            .map(|_| ())
            .ok_or_else(|| Refusal::from("there is no such account"))
    }
}

/// `message` as a sync report keeps it: one line, without control
/// characters, at most [`SYNC_MESSAGE_LIMIT`] characters.
fn sync_message(message: &str) -> String {
    message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|character| !character.is_control())
        .take(SYNC_MESSAGE_LIMIT)
        .collect()
}

/// The account the record keeps as `id`, in the shape every account type reads.
fn account_of(id: &Id, account: &ListedAccount) -> Account {
    Account {
        id: AccountId::new(id.as_str()),
        type_id: AccountTypeId::new(account.type_id.clone()),
        display_name: account.display_name.clone(),
        config: account.config.clone(),
        enabled: account.enabled,
        tint: account.tint,
    }
}

impl SharedResource for AccountList {
    const OWNER: &'static str = ACCOUNTS_OWNER;
}

impl AccountList {
    /// Every account, in the order the record keeps them.
    #[must_use]
    pub fn all(&self) -> Vec<Account> {
        self.accounts
            .iter()
            .map(|(id, account)| account_of(id, account))
            .collect()
    }

    /// The account `id`, when the record keeps it.
    #[must_use]
    pub fn account(&self, id: &str) -> Option<Account> {
        let key = Id::from_raw(id.to_owned());
        self.accounts
            .get(&key)
            .map(|account| account_of(&key, account))
    }

    /// The account `id`, when the record keeps it, as [`Account`] carries its
    /// id. The same account [`Self::account`] answers.
    #[must_use]
    pub fn get(&self, id: &AccountId) -> Option<Account> {
        self.account(id.as_str())
    }

    /// Remove the account `id`. Whether the record held it.
    pub fn remove(&mut self, id: &AccountId) -> bool {
        self.accounts.remove(&Id::from_raw(id.0.clone())).is_some()
    }

    /// Whether the record holds no account.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// How many accounts the record holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    /// How the account `id`'s sync went, when a device reported it.
    #[must_use]
    pub fn sync_of(&self, id: &str) -> Option<&AccountSync> {
        self.accounts
            .get(&Id::from_raw(id.to_owned()))
            .and_then(|account| account.sync.as_ref())
    }

    /// The edit that reports `health` for the account `id` from `device` at `at_ms`, when it
    /// changes what the record says: none when the record would say the same.
    ///
    /// # Errors
    /// The record has no such account.
    pub fn reporting(
        &self,
        id: &str,
        device: &str,
        health: SyncHealth,
        at_ms: u64,
        signed_in: bool,
        message: &str,
    ) -> Result<Option<Invoke<Self>>, Refusal> {
        let mut reported = self.clone();
        reported.report_sync(
            Id::from_raw(id.to_owned()),
            device.to_owned(),
            health,
            at_ms,
            signed_in,
            message.to_owned(),
        )?;
        Ok((reported != *self).then(|| {
            Self::REPORT_SYNC.with((
                Id::from_raw(id.to_owned()),
                device.to_owned(),
                health,
                at_ms,
                signed_in,
                message.to_owned(),
            ))
        }))
    }

    /// Every account of the kind `type_id`.
    #[must_use]
    pub fn of_type(&self, type_id: &str) -> Vec<Account> {
        self.accounts
            .iter()
            .filter(|(_, account)| account.type_id == type_id)
            .map(|(id, account)| account_of(id, account))
            .collect()
    }

    /// The accounts in `value`, the record as any reader of the workspace reads it
    /// (`record:<app>:<id>`): each account by its id.
    ///
    /// # Errors
    /// `value` does not read as the workspace's accounts.
    pub fn from_readable(value: &serde_json::Value) -> Result<Self, String> {
        let mut record = Self::default();
        let Some(accounts) = value.get("accounts") else {
            return Ok(record);
        };
        let accounts = accounts
            .as_object()
            .ok_or("the workspace's accounts do not read as accounts")?;
        for (id, entry) in accounts {
            let fields = entry
                .get(tilo_resource::collection::VALUE)
                .ok_or_else(|| format!("the workspace's account {id} reads as no account"))?;
            let text = |field: &str| {
                fields
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            record
                .keep_account(
                    id.clone(),
                    text("type_id"),
                    text("display_name"),
                    fields.get("config").cloned().unwrap_or_default(),
                    fields
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or_default(),
                    fields
                        .get("tint")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|tint| u8::try_from(tint).ok())
                        .unwrap_or_default(),
                )
                .map_err(|refusal| format!("the workspace's account {id}: {refusal}"))?;
        }
        Ok(record)
    }

    /// The edit that saves `account` as it is.
    #[must_use]
    pub fn saving(account: &Account) -> Invoke<Self> {
        // An account without connection settings is saved with none: the
        // action API refuses a missing value.
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

    /// Keep this record as the one `config_root` stages, before its workspace
    /// exists.
    ///
    /// # Errors
    /// The record cannot be built, or its directory cannot be made or the file
    /// written.
    pub fn stage_in(&self, config_root: &Path) -> std::io::Result<()> {
        let mut replica = Replica::<Self>::new().in_app(ACCOUNTS_OWNER, "Settings");
        for account in self.all() {
            replica
                .apply_confirmed(Self::saving(&account))
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
        keep_staged_in(config_root, &mut replica)
    }
}

/// Where a home keeps the workspace's accounts before it has a workspace:
/// the record itself, which an install stages for the workspace it will make.
/// The value is the record, not a second account list.
#[must_use]
pub fn staged_path_in(config_root: &Path) -> PathBuf {
    crate::accounts_dir_in(config_root).join("record")
}

/// The record staged under `config_root`, or a new one.
///
/// # Errors
/// The staged record does not read as this record.
pub fn staged_in(config_root: &Path) -> Result<Replica<AccountList, AppNamed>, String> {
    let path = staged_path_in(config_root);
    match std::fs::read(&path) {
        Ok(bytes) => Replica::load(&bytes)
            .map(|record| record.in_app(ACCOUNTS_OWNER, "Settings"))
            .map_err(|error| format!("{}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Replica::new().in_app(ACCOUNTS_OWNER, "Settings"))
        }
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Keep `record` as the record `config_root` stages.
///
/// # Errors
/// The record's directory cannot be made or the file cannot be written.
pub fn keep_staged_in(
    config_root: &Path,
    record: &mut Replica<AccountList, AppNamed>,
) -> std::io::Result<()> {
    let path = staged_path_in(config_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tilo_files::native::atomic::write(&path, &record.save())
}
