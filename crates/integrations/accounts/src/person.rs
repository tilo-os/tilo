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
    /// How its sync goes on each device that attaches it, by the device's key
    /// in hex: every device writes its own row and no other's, so one going
    /// quiet, failing or going away leaves the others as they were.
    pub syncs: Collection<AccountSync>,
}

/// How an account's sync is going on one device that attaches it.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct AccountSync {
    /// The device that reported it, by its key in hex: the row's own key.
    pub device: String,
    /// Whether the device still has the account set up, so it can check it. A
    /// device that no longer does says so before it stops.
    pub attached: bool,
    /// Whether it is checking the account now.
    pub checking: bool,
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
    version = 2,
    migrate = AccountList::upgrade,
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
            syncs: Collection::default(),
        };
        if let Some(existing) = self.accounts.get_mut(&key) {
            // Saving what an account is keeps how its sync goes on every device.
            account.syncs = std::mem::take(&mut existing.syncs);
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

    /// Report how an account's sync goes on the device `device`: what every
    /// member's window says about it. That device's row alone is written; a
    /// report saying what it already says is not written, unless it is a
    /// success more than ten minutes newer than the one kept.
    ///
    /// Whoever may save an account may report its sync. Which device a call
    /// reports for is decided where the call comes in, never by the caller.
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
        let previous = self.existing_row(&account, &device)?;
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
            ..previous
        };
        let unchanged = previous.health == next.health
            && previous.message == next.message
            && previous.signed_in_ms.is_some() == next.signed_in_ms.is_some()
            && !(succeeded
                && at_ms
                    >= previous
                        .succeeded_ms
                        .unwrap_or_default()
                        .saturating_add(SYNC_SUCCESS_REFRESH_MS));
        if !unchanged {
            self.keep_row(&account, next);
        }
        Ok(())
    }

    /// Say whether the device `device` still has the account set up, and whether
    /// it is checking it now: its own row alone changes.
    #[action]
    pub fn report_attachment(
        &mut self,
        #[param("The account's id")] account: Id,
        #[param("The device, by its key in hex")] device: String,
        #[param("Whether it still has the account set up")] attached: bool,
        #[param("Whether it is checking the account now")] checking: bool,
    ) -> Result<(), Refusal> {
        let previous = self.existing_row(&account, &device)?;
        if previous.attached != attached || previous.checking != checking {
            self.keep_row(
                &account,
                AccountSync {
                    device,
                    attached,
                    checking,
                    ..previous
                },
            );
        }
        Ok(())
    }

    /// The row `device` keeps of the account `account`, or an empty one it never
    /// reported.
    ///
    /// # Errors
    /// The record has no such account, or the report names no device.
    fn existing_row(&self, account: &Id, device: &str) -> Result<AccountSync, Refusal> {
        if device.trim().is_empty() {
            return Err(Refusal::from("a sync report names the device it is from"));
        }
        let listed = self
            .accounts
            .get(account)
            .ok_or_else(|| Refusal::from("there is no such account"))?;
        Ok(listed
            .syncs
            .get(&Id::from_raw(device.to_owned()))
            .cloned()
            .unwrap_or_default())
    }

    /// Keep `row` as `account`'s row for its device: no other device's row changes.
    fn keep_row(&mut self, account: &Id, row: AccountSync) {
        if let Some(listed) = self.accounts.get_mut(account) {
            listed.syncs.push(Id::from_raw(row.device.clone()), row);
        }
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

impl AccountList {
    /// Bring a record written before each device kept its own row up to this version: the one
    /// row an account kept, from whichever device reported last, becomes that device's row, and
    /// no other device is taken to have it.
    ///
    /// # Errors
    /// The record is of no version this knows, or its document cannot be read or changed.
    fn upgrade(doc: &mut tilo_resource::Doc, from: u32) -> Result<(), tilo_resource::ModelError> {
        use automerge::transaction::Transactable as _;
        use automerge::{ObjType, ROOT, ReadDoc as _, ScalarValue, Value};
        if from != 1 {
            return Err(tilo_resource::ModelError::invalid(format!(
                "no migration from {from}"
            )));
        }
        let Some((Value::Object(_), accounts)) = doc.get(&ROOT, "accounts")? else {
            return Ok(());
        };
        for entry in doc.keys(&accounts).collect::<Vec<_>>() {
            let Some((Value::Object(_), holder)) = doc.get(&accounts, entry.as_str())? else {
                continue;
            };
            let Some((Value::Object(_), value)) = doc.get(&holder, "v")? else {
                continue;
            };
            let Some((Value::Object(_), one)) = doc.get(&value, "sync")? else {
                continue;
            };
            let Some((Value::Scalar(device), _)) = doc.get(&one, "device")? else {
                continue;
            };
            let ScalarValue::Str(device) = device.as_ref() else {
                continue;
            };
            let device = device.to_string();
            if device.is_empty() {
                continue;
            }
            let existing = match doc.get(&value, "syncs")? {
                Some((Value::Object(_), syncs)) => Some(syncs),
                _ => None,
            };
            let syncs = match existing {
                Some(syncs) => syncs,
                None => doc.put_object(&value, "syncs", ObjType::Map)?,
            };
            let entry = doc.put_object(&syncs, device.as_str(), ObjType::Map)?;
            doc.put(&entry, "k", ScalarValue::Str(device.as_str().into()))?;
            let row = doc.put_object(&entry, "v", ObjType::Map)?;
            for field in doc.keys(&one).collect::<Vec<_>>() {
                let scalar = match doc.get(&one, field.as_str())? {
                    Some((Value::Scalar(scalar), _)) => Some(scalar.into_owned()),
                    _ => None,
                };
                if let Some(scalar) = scalar {
                    doc.put(&row, field, scalar)?;
                }
            }
            doc.delete(&value, "sync")?;
        }
        Ok(())
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

    /// How the account `id`'s sync goes on each device that reported it, in the
    /// order the record keeps the rows: every device's own, never just this one's.
    #[must_use]
    pub fn syncs_of(&self, id: &str) -> Vec<&AccountSync> {
        let Some(account) = self.accounts.get(&Id::from_raw(id.to_owned())) else {
            return Vec::new();
        };
        account.syncs.iter().map(|(_, sync)| sync).collect()
    }

    /// How the account `id`'s sync goes on `device`, when that device reported it.
    #[must_use]
    pub fn sync_of(&self, id: &str, device: &str) -> Option<&AccountSync> {
        self.accounts
            .get(&Id::from_raw(id.to_owned()))
            .and_then(|account| account.syncs.get(&Id::from_raw(device.to_owned())))
    }

    /// How the account `id`'s sync goes on the device that tried most
    /// recently, when any device reported it: what a row says before it names
    /// a device.
    #[must_use]
    pub fn latest_of(&self, id: &str) -> Option<&AccountSync> {
        self.accounts
            .get(&Id::from_raw(id.to_owned()))
            .and_then(|account| {
                account
                    .syncs
                    .iter()
                    .map(|(_, sync)| sync)
                    .max_by_key(|sync| sync.attempted_ms)
            })
    }

    /// Whether any device says it still has the account `id` set up, so it
    /// can check it.
    #[must_use]
    pub fn attached_to(&self, id: &str) -> bool {
        self.syncs_of(id).into_iter().any(|sync| sync.attached)
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

    /// The edit that says whether `device` still has the account `id` set up, and whether it is
    /// checking it now, when it changes what the record says.
    ///
    /// # Errors
    /// The record has no such account.
    pub fn reporting_attachment(
        &self,
        id: &str,
        device: &str,
        attached: bool,
        checking: bool,
    ) -> Result<Option<Invoke<Self>>, Refusal> {
        let mut reported = self.clone();
        reported.report_attachment(
            Id::from_raw(id.to_owned()),
            device.to_owned(),
            attached,
            checking,
        )?;
        Ok((reported != *self).then(|| {
            Self::REPORT_ATTACHMENT.with((
                Id::from_raw(id.to_owned()),
                device.to_owned(),
                attached,
                checking,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::AppNamed;
    use tilo_resource::Replica;

    #[test]
    fn browser_picker_projects_settings_accounts_without_crossing_identity_or_origin() {
        let root = "ab".repeat(32);
        let origin = "https://host.example";
        let mut record = Replica::<AccountList>::new().in_app("so.tilo.model", "Model");
        for (id, key, host, enabled) in [
            ("work", root.clone(), origin, true),
            ("disabled", root.clone(), origin, false),
            ("other-root", "cd".repeat(32), origin, true),
            ("other-host", root.clone(), "https://other.example", true),
        ] {
            record
                .apply(AccountList::SAVE_ACCOUNT.with((
                    id.into(),
                    TILO_IDENTITY_TYPE.into(),
                    id.into(),
                    serde_json::json!({
                        "userPublicKey": key, "origin": host, "privateField": "never-exported"
                    }),
                    enabled,
                    0,
                )))
                .unwrap();
        }
        let state = record.state().unwrap();
        let rows = state.browser_accounts(&root, origin);
        assert_eq!(
            rows,
            vec![BrowserAccount {
                id: "work".into(),
                display_name: "work".into(),
                user_public_key: root.clone()
            }]
        );
        assert!(
            !serde_json::to_string(&rows)
                .unwrap()
                .contains("privateField")
        );
        assert!(
            state
                .browser_accounts(&root, "http://host.example")
                .is_empty()
        );
        assert!(state.browser_accounts("invalid", origin).is_empty());
        record
            .apply(AccountList::REMOVE_ACCOUNT.with(Id::from_raw("work")))
            .unwrap();
        assert!(
            record
                .state()
                .unwrap()
                .browser_accounts(&root, origin)
                .is_empty()
        );
    }

    #[test]
    fn identity_publication_uses_the_shared_account_record_and_preserves_disabled_choices() {
        let root = "ab".repeat(32);
        let origin = "https://host.example";
        let mut record = Replica::<AccountList>::new().in_app("so.tilo.model", "Model");
        let publication = AccountList::PUBLISH_BROWSER_IDENTITY.with((
            root.clone(),
            origin.into(),
            "Work".into(),
        ));
        record.apply(publication.clone()).unwrap();
        record.apply(publication.clone()).unwrap();
        let choices = record.state().unwrap().browser_accounts(&root, origin);
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].display_name, "Work");
        let id = choices[0].id.clone();
        let account = record.state().unwrap().account(&id).unwrap();
        assert_eq!(
            account.config,
            serde_json::json!({"userPublicKey": root, "origin": origin})
        );
        record
            .apply(AccountList::SAVE_ACCOUNT.with((
                id.clone(),
                TILO_IDENTITY_TYPE.into(),
                "Private".into(),
                account.config,
                false,
                0,
            )))
            .unwrap();
        record.apply(publication).unwrap();
        assert!(
            record
                .state()
                .unwrap()
                .browser_accounts(&root, origin)
                .is_empty()
        );
        assert_eq!(
            record.state().unwrap().account(&id).unwrap().display_name,
            "Private"
        );
        for invalid in [
            "http://host.example",
            "https://host.example/path",
            "https://name:secret@host.example",
            "https://host.example/?key=value",
        ] {
            assert!(
                record
                    .apply(AccountList::PUBLISH_BROWSER_IDENTITY.with((
                        root.clone(),
                        invalid.into(),
                        "Work".into()
                    )))
                    .is_err()
            );
        }
        assert_eq!(record.state().unwrap().len(), 1);
    }

    fn save(replica: &mut Replica<AccountList, AppNamed>, id: &str, type_id: &str, host: &str) {
        replica
            .apply(AccountList::SAVE_ACCOUNT.with((
                id.into(),
                type_id.into(),
                format!("{id} inbox"),
                serde_json::json!({ "host": host }),
                true,
                2,
            )))
            .unwrap();
    }

    #[test]
    fn accounts_are_saved_by_id_and_removed() {
        let mut replica = Replica::<AccountList>::new().in_app("so.tilo.model", "Model");
        save(&mut replica, "a@x.example", "email", "imap.x");
        save(&mut replica, "b@y.example", "email", "imap.y");
        save(&mut replica, "work", "caldav", "dav.example");
        save(&mut replica, "a@x.example", "email", "imap.new");

        let accounts = replica.state().unwrap();
        assert_eq!(accounts.of_type("email").len(), 2, "saving again replaces");
        assert_eq!(
            accounts.account("a@x.example").unwrap().config["host"],
            "imap.new"
        );

        replica
            .apply(AccountList::REMOVE_ACCOUNT.with(Id::from_raw("b@y.example")))
            .unwrap();
        let accounts = replica.state().unwrap();
        assert!(accounts.account("b@y.example").is_none());
        assert_eq!(
            accounts.of_type("email").len(),
            1,
            "the other account stays"
        );
        assert!(
            replica
                .apply(AccountList::REMOVE_ACCOUNT.with(Id::from_raw("gone")))
                .is_err()
        );
    }

    /// The edit that reports `health` for `id` from `device` at `at_ms`.
    fn report(
        id: &str,
        device: &str,
        health: SyncHealth,
        at_ms: u64,
        message: &str,
    ) -> Invoke<AccountList> {
        AccountList::REPORT_SYNC.with((
            Id::from_raw(id),
            device.into(),
            health,
            at_ms,
            false,
            message.into(),
        ))
    }

    /// The edit that says whether `device` still has `id` set up and is checking it now.
    fn attached(
        id: &str,
        device: &str,
        still_attached: bool,
        checking: bool,
    ) -> Invoke<AccountList> {
        AccountList::REPORT_ATTACHMENT.with((
            Id::from_raw(id),
            device.into(),
            still_attached,
            checking,
        ))
    }

    /// Every member reads how an account's sync goes on every device that
    /// attached it: one device's report writes its own row and leaves the
    /// other's as it was, a report saying nothing new is not written, a
    /// success is again once it is ten minutes newer, and saving the account
    /// keeps every row.
    #[test]
    fn every_device_that_attached_an_account_keeps_its_own_row() {
        let mut replica = Replica::<AccountList>::new().in_app("so.tilo.model", "Model");
        save(&mut replica, "work", "caldav", "dav.example");
        let minute = 60_000;
        let succeeded = |replica: &Replica<AccountList, AppNamed>, device: &str| {
            replica
                .state()
                .unwrap()
                .sync_of("work", device)
                .and_then(|sync| sync.succeeded_ms)
        };
        replica
            .apply(report(
                "work",
                "laptop",
                SyncHealth::Healthy,
                minute,
                "Up to date",
            ))
            .unwrap();
        replica
            .apply(report(
                "work",
                "phone",
                SyncHealth::Offline,
                2 * minute,
                "Waiting\nfor a connection\u{7}",
            ))
            .unwrap();
        let rows = replica.state().unwrap().syncs_of("work").len();
        assert_eq!(rows, 2, "both devices' rows are kept");
        let phone = replica
            .state()
            .unwrap()
            .sync_of("work", "phone")
            .cloned()
            .unwrap();
        assert_eq!(phone.device, "phone");
        assert_eq!(phone.health, SyncHealth::Offline);
        assert_eq!(
            phone.message, "Waiting for a connection",
            "one line, no control characters"
        );
        assert_eq!(
            succeeded(&replica, "laptop"),
            Some(minute),
            "the laptop's success stands beside the phone's failure"
        );
    }

    /// A success more than ten minutes newer is written again, a report that
    /// changes nothing is not, and saving the account keeps what every device's
    /// row says.
    #[test]
    fn a_success_ten_minutes_newer_is_written_again_and_saving_keeps_the_rows() {
        let mut replica = Replica::<AccountList>::new().in_app("so.tilo.model", "Model");
        save(&mut replica, "work", "caldav", "dav.example");
        let minute = 60_000;
        let succeeded = |replica: &Replica<AccountList, AppNamed>, device: &str| {
            replica
                .state()
                .unwrap()
                .sync_of("work", device)
                .and_then(|sync| sync.succeeded_ms)
        };
        replica
            .apply(report(
                "work",
                "laptop",
                SyncHealth::Healthy,
                minute,
                "Up to date",
            ))
            .unwrap();
        replica
            .apply(report(
                "work",
                "laptop",
                SyncHealth::Healthy,
                3 * minute,
                "Up to date",
            ))
            .unwrap();
        let written = replica.save();
        replica
            .apply(report(
                "work",
                "laptop",
                SyncHealth::Healthy,
                5 * minute,
                "Up to date",
            ))
            .unwrap();
        assert_eq!(
            succeeded(&replica, "laptop"),
            Some(minute),
            "the same success minutes later is not written, the phone's row being another's"
        );
        assert_eq!(replica.save(), written, "nothing new is written");
        replica
            .apply(report(
                "work",
                "laptop",
                SyncHealth::Healthy,
                14 * minute,
                "Up to date",
            ))
            .unwrap();
        assert_eq!(succeeded(&replica, "laptop"), Some(14 * minute));

        let long = "x".repeat(400);
        replica
            .apply(report(
                "work",
                "laptop",
                SyncHealth::Error,
                15 * minute,
                &long,
            ))
            .unwrap();
        save(&mut replica, "work", "caldav", "dav.moved.example");
        let sync = replica
            .state()
            .unwrap()
            .sync_of("work", "laptop")
            .cloned()
            .unwrap();
        assert_eq!(
            sync.health,
            SyncHealth::Error,
            "saving the account keeps every device's row"
        );
        assert_eq!(sync.message.chars().count(), SYNC_MESSAGE_LIMIT);
        assert!(
            replica
                .apply(report("gone", "laptop", SyncHealth::Healthy, minute, ""))
                .is_err(),
            "no report for an account the workspace does not have"
        );
    }

    /// A record written before each device kept its own row keeps the row it had: it becomes
    /// the row of the device that wrote it, and reading it is that device's.
    #[test]
    fn an_older_records_one_row_becomes_the_row_of_the_device_that_wrote_it() {
        use automerge::transaction::Transactable as _;
        use automerge::{ObjType, ROOT, ScalarValue};
        use tilo_resource::Resource as _;
        let mut doc = tilo_resource::Doc::new();
        let accounts = doc.put_object(&ROOT, "accounts", ObjType::Map).unwrap();
        let entry = doc.put_object(&accounts, "work", ObjType::Map).unwrap();
        doc.put(&entry, "k", ScalarValue::Str("work".into()))
            .unwrap();
        let account = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        doc.put(&account, "type_id", ScalarValue::Str("caldav".into()))
            .unwrap();
        doc.put(&account, "display_name", ScalarValue::Str("Work".into()))
            .unwrap();
        doc.put(&account, "enabled", ScalarValue::Boolean(true))
            .unwrap();
        doc.put(&account, "tint", ScalarValue::Uint(2)).unwrap();
        let one = doc.put_object(&account, "sync", ObjType::Map).unwrap();
        doc.put(&one, "device", ScalarValue::Str("laptop".into()))
            .unwrap();
        doc.put(&one, "health", ScalarValue::Str("healthy".into()))
            .unwrap();
        doc.put(&one, "attempted_ms", ScalarValue::Uint(60_000))
            .unwrap();
        doc.put(&one, "succeeded_ms", ScalarValue::Uint(60_000))
            .unwrap();
        doc.put(&one, "message", ScalarValue::Str("Up to date".into()))
            .unwrap();

        AccountList::upgrade(&mut doc, 1).unwrap();
        tilo_resource::evolution::write_version(&mut doc, 2).unwrap();
        let read = AccountList::read(&doc).unwrap();
        let rows = read.syncs_of("work");
        assert_eq!(rows.len(), 1, "the one row it had is the one it has");
        assert_eq!(rows[0].device, "laptop");
        assert_eq!(rows[0].health, SyncHealth::Healthy);
        assert_eq!(rows[0].succeeded_ms, Some(60_000_u64));
        assert!(!rows[0].attached, "no device is taken to have it anew");
    }

    /// A device that no longer has the account says so, and one that is
    /// checking it now says that; the account is attached while any device
    /// says it has it.
    #[test]
    fn a_device_says_whether_it_still_attaches_an_account_and_whether_it_is_checking() {
        let mut replica = Replica::<AccountList>::new().in_app("so.tilo.model", "Model");
        save(&mut replica, "work", "caldav", "dav.example");
        replica
            .apply(report("work", "laptop", SyncHealth::Healthy, 1, ""))
            .unwrap();
        replica
            .apply(attached("work", "laptop", true, false))
            .unwrap();
        replica
            .apply(report("work", "phone", SyncHealth::Healthy, 1, ""))
            .unwrap();
        replica
            .apply(attached("work", "phone", true, true))
            .unwrap();
        {
            let accounts = replica.state().unwrap();
            assert!(accounts.attached_to("work"));
            assert!(
                accounts
                    .sync_of("work", "phone")
                    .is_some_and(|sync| sync.checking),
                "the phone is checking now"
            );
        }
        replica
            .apply(attached("work", "phone", false, false))
            .unwrap();
        let accounts = replica.state().unwrap();
        assert!(
            accounts.attached_to("work"),
            "the laptop still has the account"
        );
        assert!(
            !accounts
                .sync_of("work", "phone")
                .is_some_and(|sync| sync.attached),
            "the phone says it no longer has it"
        );
    }

    /// A sync report is a change of the same kind as saving an account, so
    /// the grant that saves one reports its sync and nothing less does.
    #[test]
    fn whoever_saves_an_account_reports_its_sync() {
        use tilo_resource::Operation as _;
        let reporting = report("work", "laptop", SyncHealth::Healthy, 1, "")
            .build(1)
            .unwrap();
        let saving = saving("work", "caldav", calendar("https://dav.example/"))
            .build(1)
            .unwrap();
        assert_eq!(reporting.operation.kind(), saving.operation.kind());
    }

    /// The edit that saves the account `id` of `type_id` with `config`.
    fn saving(id: &str, type_id: &str, config: serde_json::Value) -> Invoke<AccountList> {
        AccountList::SAVE_ACCOUNT.with((id.into(), type_id.into(), "Work".into(), config, true, 0))
    }

    /// What the account `id` keeps as its settings in `replica`; none when
    /// it is gone.
    fn kept(replica: &Replica<AccountList, AppNamed>, id: &str) -> Option<serde_json::Value> {
        replica
            .state()
            .unwrap()
            .accounts
            .get(&Id::from_raw(id))
            .map(|account| account.config.clone())
    }

    /// Whoever acts for a member, an agent included, changes the workspace's
    /// accounts as the member may: what an account is, where it signs in, a
    /// new account, a removal, and an account no longer connected for
    /// reading only.
    #[test]
    fn an_agent_acting_for_a_member_changes_the_workspaces_accounts() {
        let person = "ab".repeat(32);
        let mut theirs = workspace_accounts(&person);
        let mut agent = Replica::<AccountList>::load(&theirs.save())
            .unwrap()
            .in_app("so.tilo.model", "Model")
            .as_agent_for("helper", &person);
        let elsewhere = "imap.elsewhere.example";
        agent
            .apply(saving("ada@example.test", "email", mail(elsewhere)))
            .unwrap();
        assert_eq!(kept(&agent, "ada@example.test"), Some(mail(elsewhere)));
        agent
            .apply(saving(
                "work",
                "webdav",
                calendar("https://dav.elsewhere.example/"),
            ))
            .unwrap();
        assert_eq!(
            kept(&agent, "work"),
            Some(calendar("https://dav.elsewhere.example/"))
        );
        agent
            .apply(saving("news@example.test", "email", mail(elsewhere)))
            .unwrap();
        assert_eq!(kept(&agent, "news@example.test"), Some(mail(elsewhere)));
        agent
            .apply(AccountList::REMOVE_ACCOUNT.with(Id::from_raw("ada@example.test")))
            .unwrap();
        assert_eq!(kept(&agent, "ada@example.test"), None);
        let reading_only = serde_json::json!({
            "server_url": "https://dav.example/", "username": "ada", "read_only": true,
        });
        agent.apply(saving("read", "caldav", reading_only)).unwrap();
        agent
            .apply(saving("read", "caldav", calendar("https://dav.example/")))
            .unwrap();
        assert_eq!(kept(&agent, "read"), Some(calendar("https://dav.example/")));
    }

    /// A mail account's settings, reading at `incoming`.
    fn mail(incoming: &str) -> serde_json::Value {
        serde_json::json!({"incoming": incoming, "outgoing": "smtp.example", "username": "ada"})
    }

    /// A calendar account's settings, at `address`.
    fn calendar(address: &str) -> serde_json::Value {
        serde_json::json!({"server_url": address, "username": "ada"})
    }

    /// The workspace's accounts as a member's device keeps them: a mail
    /// account, a calendar account, and one added and removed again.
    fn workspace_accounts(person: &str) -> Replica<AccountList, AppNamed> {
        let mut theirs = Replica::<AccountList>::new()
            .in_app("so.tilo.model", "Model")
            .as_invoker(person);
        theirs
            .apply(saving("ada@example.test", "email", mail("imap.example")))
            .unwrap();
        theirs
            .apply(saving("work", "caldav", calendar("https://dav.example/")))
            .unwrap();
        theirs
            .apply(saving("old@example.test", "email", mail("imap.example")))
            .unwrap();
        theirs
            .apply(AccountList::REMOVE_ACCOUNT.with(Id::from_raw("old@example.test")))
            .unwrap();
        theirs
    }

    /// The device's own code, with nobody invoking, and a member at their
    /// device change the workspace's accounts as before.
    #[test]
    fn a_members_own_device_changes_the_accounts_as_before() {
        let person = "ab".repeat(32);
        let mut theirs = workspace_accounts(&person);
        let mut device = Replica::<AccountList>::load(&theirs.save())
            .unwrap()
            .in_app("so.tilo.model", "Model");
        device
            .apply(saving("news@example.test", "email", mail("imap.example")))
            .unwrap();
        device
            .apply(saving(
                "news@example.test",
                "email",
                mail("imap.moved.example"),
            ))
            .unwrap();
        device
            .apply(AccountList::REMOVE_ACCOUNT.with(Id::from_raw("news@example.test")))
            .unwrap();
        assert_eq!(kept(&device, "news@example.test"), None);

        theirs
            .apply(saving(
                "ada@example.test",
                "email",
                mail("imap.moved.example"),
            ))
            .unwrap();
        assert_eq!(
            kept(&theirs, "ada@example.test"),
            Some(mail("imap.moved.example"))
        );
        theirs
            .apply(AccountList::REMOVE_ACCOUNT.with(Id::from_raw("work")))
            .unwrap();
        assert_eq!(kept(&theirs, "work"), None);
    }

    /// The workspace's roles decide who changes its accounts, as they decide
    /// any workspace action: an Editor, whether their own device or an agent
    /// they grant Mail, adds and removes accounts; a Viewer is refused.
    #[test]
    fn an_editor_changes_the_accounts_and_a_viewer_is_refused() {
        use tilo_action_policy::{
            Answer, Authority, Context, Grant, Grantee, Role, TreePath, WorkspacePolicy,
        };
        use tilo_action_types::{ActionKind, KindSet};
        use tilo_member_types::Acting;
        use tilo_resource::Resource as _;
        const WORKSPACE: &str = "w";
        const MAIL: &str = "so.tilo.mail";
        let editor = "ab".repeat(32);
        let viewer = "cd".repeat(32);
        let mut policy = WorkspacePolicy::new(WORKSPACE, &editor);
        policy.members.insert(editor.clone(), Role::Editor.into());
        policy.members.insert(viewer.clone(), Role::Viewer.into());
        // Mail publishes the accounts record, so its actions are Mail's.
        let accounts = TreePath::workspace(WORKSPACE)
            .app(MAIL)
            .kind(AccountList::KIND);
        let cx = Context::at_secs(1);
        let answer = |acting: &Acting, granted: &[Grant]| {
            Authority::of_acting(acting, &policy, granted)
                .decide(&accounts, ActionKind::Undoable, &cx)
                .answer
        };
        assert_eq!(answer(&Acting::person(&editor), &[]), Answer::Allow);
        assert_eq!(answer(&Acting::person(&viewer), &[]), Answer::Deny);
        // The grant an agent's turn gives it, on Mail.
        let mail = [Grant::new(
            "turn:helper:0",
            Grantee::agent("helper", &editor),
            TreePath::workspace(WORKSPACE).app(MAIL),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Grantee::person(&editor),
        )];
        assert_eq!(
            answer(
                &Acting::agent("helper", &editor).acting_for(Acting::person(&editor)),
                &mail
            ),
            Answer::Allow
        );
        assert_eq!(
            answer(
                &Acting::agent("helper", &viewer).acting_for(Acting::person(&viewer)),
                &mail
            ),
            Answer::Deny
        );
    }
}
