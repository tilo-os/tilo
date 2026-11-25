//! A workspace's connected accounts as the apps this host serves read them: the mail, calendar
//! and cloud-storage accounts its members connected, each with the kind of service it is, the
//! address it signs in at, the provider it belongs to and how its last sync went.
//!
//! The reading is the accounts record's ([`crate::workspaces::account_logins`]), which the
//! workspace keeps as one document: the same record
//! [`list`](tilo_actions::remote::person_accounts::LIST) answers, and it carries no secret. A
//! secret stays in the workspace's vault and is named by the account's settings alone, so nothing
//! here reads one back. Changing an account is the record's own actions
//! ([`tilo_actions::remote::person_accounts`]), answered where the workspace runs.

use serde::Serialize;
use serde_json::Value;

use super::Workspaces;
use super::person_settings;
use super::person_settings::cloud_control::Mount;

impl Workspaces {
    /// The accounts `workspace` keeps, as the apps this host serves read them: the same record
    /// its members' windows change, and no secret in it. A workspace whose accounts this device
    /// does not hold yet lists none.
    pub async fn accounts_reading(&self, workspace: &str) -> Value {
        reading(self, workspace).await
    }
}

/// The accounts `workspace` keeps, as the apps this host serves read them: the same record its
/// members' windows change, and no secret in it. A workspace whose accounts this device does not
/// hold yet lists none.
async fn reading(workspaces: &Workspaces, workspace: &str) -> Value {
    let held = workspaces.workspace_accounts(workspace).await.ok();
    // The device's mounts say which of its accounts are paused; a service that does not
    // answer leaves every account as its record says.
    let cloud = workspaces.cloud_control();
    let mounts = tokio::task::spawn_blocking(move || cloud.mounts())
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let accounts = held
        .as_ref()
        .map(|held| {
            held.all()
                .iter()
                .filter_map(|account| view_of(held, account, &mounts))
                .collect()
        })
        .unwrap_or_default();
    let reading = Reading { accounts };
    serde_json::to_value(reading).unwrap_or(Value::Null)
}

/// A workspace's accounts, as one reading.
#[derive(Debug, Serialize)]
struct Reading {
    accounts: Vec<View>,
}

/// One account of the workspace's, without its secret.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct View {
    /// The account's id, as every action names it.
    id: String,
    /// The kind of service it is.
    kind: Kind,
    /// What its person calls it.
    name: String,
    /// The address or name it signs in at.
    address: String,
    /// The name it signs in with.
    username: String,
    /// The provider it belongs to.
    provider: String,
    /// How its last sync went.
    status: Status,
    /// What a person reads about that, when the account's device said something.
    words: String,
    /// Whether the account is in use.
    enabled: bool,
    /// A mail account's incoming server as a person writes it.
    incoming: String,
    /// Its outgoing server as a person writes it.
    outgoing: String,
    /// The other addresses a mail account sends as, each `Name <address>`.
    identities: Vec<String>,
    /// A calendar account's calendars and which of them show.
    calendars: Vec<CalendarView>,
    /// Which of its services the account feeds.
    services: ServicesView,
    /// A cloud-storage account's files: `sync` kept in sync, `import` copied into the
    /// workspace once; empty for an account that mounts no files.
    files_mode: String,
    /// Whether its background sync and uploads are paused on this device.
    paused: bool,
}

/// Which of an account's services its person uses, as its row reads them.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServicesView {
    mail: bool,
    calendar: bool,
    files: bool,
}

/// One of a calendar account's calendars, as its person chooses which show.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CalendarView {
    /// Its id, as its server keeps it and the selection names it.
    id: String,
    /// What its server calls it.
    name: String,
    /// Whether its events show in the calendar.
    visible: bool,
    /// Whether its server lets this workspace change it.
    read_only: bool,
}

/// The kinds of account the reading lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Kind {
    Mail,
    Calendar,
    Cloud,
    Other,
}

/// How an account's last sync went, as a person reads it on the account's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Status {
    /// Up to date.
    Connected,
    /// Its person has to sign in again.
    NeedsReconnect,
    /// Its last sync failed.
    Error,
    /// Waiting for a connection.
    Waiting,
    /// Syncing now.
    Syncing,
    /// Nothing has synced it yet.
    Never,
}

/// The kind `type_id` names; a kind this build does not know is [`Kind::Other`].
fn kind_of(type_id: &str) -> Kind {
    use accounts::cloud_types;
    if type_id == accounts::email::TYPE_ID {
        return Kind::Mail;
    }
    if type_id == accounts::calendar::TYPE_ID {
        return Kind::Calendar;
    }
    if [
        cloud_types::GOOGLE_DRIVE,
        cloud_types::ONEDRIVE,
        cloud_types::DROPBOX,
        cloud_types::ICLOUD_DRIVE,
        cloud_types::NEXTCLOUD,
        cloud_types::WEBDAV,
    ]
    .contains(&type_id)
    {
        return Kind::Cloud;
    }
    // A kind this build does not know still signs in somewhere, and its person may remove it.
    Kind::Other
}

/// One account as the reading shows it, or `None` for an account it does not list.
fn view_of(
    held: &accounts::person::AccountList,
    account: &accounts::Account,
    mounts: &[Mount],
) -> Option<View> {
    if account.type_id.as_str() == accounts::person::TILO_IDENTITY_TYPE {
        return None;
    }
    let kind = kind_of(account.type_id.as_str());
    let address = address_of(account);
    let name = if account.display_name.trim().is_empty() {
        address.clone()
    } else {
        account.display_name.trim().to_owned()
    };
    let sync = held.sync_of(account.id.as_str());
    let health = sync.map_or(accounts::SyncHealth::Never, |sync| sync.health);
    let status = match health {
        accounts::SyncHealth::Healthy => Status::Connected,
        accounts::SyncHealth::NeedsSignIn => Status::NeedsReconnect,
        accounts::SyncHealth::Error => Status::Error,
        accounts::SyncHealth::Offline => Status::Waiting,
        accounts::SyncHealth::Syncing => Status::Syncing,
        accounts::SyncHealth::Never => Status::Never,
    };
    let said = sync
        .map(|sync| sync.message.trim())
        .filter(|message| !message.is_empty())
        .map_or_else(|| health.summary().to_owned(), str::to_owned);
    let services = person_settings::services_of(account);
    Some(View {
        id: account.id.to_string(),
        kind,
        name,
        address: address.clone(),
        username: account
            .config
            .get("username")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map_or_else(|| address.clone(), str::to_owned),
        provider: person_settings::provider_key(account),
        status,
        words: said,
        enabled: account.enabled,
        incoming: mail_server(&account.config, "imap_host", "imap_port", 993),
        outgoing: mail_server(&account.config, "smtp_host", "smtp_port", 465),
        identities: account
            .config
            .get("identities")
            .and_then(Value::as_array)
            .map(|identities| {
                identities
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        calendars: account
            .config
            .get("calendars")
            .and_then(Value::as_array)
            .map(|calendars| {
                calendars
                    .iter()
                    .filter_map(|calendar| {
                        Some(CalendarView {
                            id: calendar.get("id")?.as_str()?.to_owned(),
                            name: calendar
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            visible: calendar
                                .get("visible")
                                .and_then(Value::as_bool)
                                .unwrap_or(true),
                            read_only: calendar
                                .get("read_only")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        services: ServicesView {
            mail: services.mail,
            calendar: services.calendar,
            files: services.files,
        },
        files_mode: account
            .config
            .get(accounts::cloud_types::FILES_MODE_KEY)
            .and_then(Value::as_str)
            .unwrap_or(if kind == Kind::Cloud { "sync" } else { "" })
            .to_owned(),
        paused: mounts
            .iter()
            .any(|mount| mount.account == account.id.as_str() && mount.paused),
    })
}

/// The address or name `account` signs in at: its mail address where it has one, else the name
/// it signs in with, else the label its provider gave it.
fn address_of(account: &accounts::Account) -> String {
    for key in ["email", "username", "label"] {
        if let Some(text) = account
            .config
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return text.to_owned();
        }
    }
    account.id.to_string()
}

/// The mail server `host_key` and `port_key` of `config` name, as a person writes it: its name,
/// and its port after a colon when that is not the usual one.
fn mail_server(config: &Value, host_key: &str, port_key: &str, usual: u64) -> String {
    let host = config
        .get(host_key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if host.is_empty() {
        return String::new();
    }
    let named = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = config
        .get(port_key)
        .and_then(Value::as_u64)
        .unwrap_or(usual);
    if port == usual {
        named
    } else {
        format!("{named}:{port}")
    }
}
