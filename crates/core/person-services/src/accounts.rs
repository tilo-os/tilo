//! A workspace's connected accounts as the apps this host serves read them: the mail, calendar
//! and cloud-storage accounts its members connected, each with the kind of service it is, the
//! address it signs in at, the provider it belongs to and how its last sync went.
//!
//! The reading is the accounts record's ([`crate::account_logins`]), which the
//! workspace keeps as one document: the same record
//! [`list`](tilo_workspace_records::accounts::LIST) answers, and it carries no secret. A
//! secret stays in the workspace's vault and is named by the account's settings alone, so nothing
//! here reads one back. Changing an account is the record's own actions
//! ([`tilo_workspace_records::accounts`]), answered where the workspace runs.

use serde_json::Value;

use crate::PersonServices as Workspaces;
use crate::person_settings;
use crate::person_settings::cloud_control::Mount;
use tilo_workspace_records::accounts::{
    AccountDevice, AccountKind as Kind, AccountStatus as Status, AccountView as View, Accounts,
    CalendarView,
};

impl crate::PersonServices {
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
    // The accounts this device's person connected at issuers their apps describe: theirs alone,
    // kept in their own workspace, so a device reads its own person's and nobody else's.
    let own = workspaces.acting_or_own(None);
    let connected = match own.person_id() {
        Some(person) => workspaces.connected_accounts(person).await,
        None => Vec::new(),
    };
    let reading = Accounts {
        accounts,
        connected,
        now_ms: tilo_clock::unix_now_ms(),
    };
    serde_json::to_value(reading).unwrap_or(Value::Null)
}

/// How one device's sync stands, as a person reads it on the account's row.
fn status_of(health: accounts::SyncHealth) -> Status {
    match health {
        accounts::SyncHealth::Healthy => Status::Connected,
        accounts::SyncHealth::NeedsSignIn => Status::NeedsReconnect,
        accounts::SyncHealth::Error => Status::Error,
        accounts::SyncHealth::Offline => Status::Waiting,
        accounts::SyncHealth::Syncing => Status::Syncing,
        accounts::SyncHealth::Never => Status::Never,
    }
}

/// What a person reads about one device's sync: what that device said, or the
/// summary of a health it named none for.
fn words_of(health: accounts::SyncHealth, message: &str) -> String {
    let said = message.trim();
    if said.is_empty() {
        health.summary().to_owned()
    } else {
        said.to_owned()
    }
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
    let (kind, address) = (kind_of(account.type_id.as_str()), address_of(account));
    let name = if account.display_name.trim().is_empty() {
        address.clone()
    } else {
        account.display_name.trim().to_owned()
    };
    let syncs = held.syncs_of(account.id.as_str());
    let devices: Vec<AccountDevice> = syncs
        .iter()
        .map(|sync| AccountDevice {
            device: sync.device.clone(),
            attached: sync.attached,
            checking: sync.checking,
            health: status_of(sync.health),
            attempted_ms: sync.attempted_ms,
            checked_ms: sync.succeeded_ms.unwrap_or_default(),
            words: words_of(sync.health, &sync.message),
        })
        .collect();
    // What the account's one row says, from every device's rows: the newest attempt is how it
    // last went. A device that went away says what its last try did but claims nothing now.
    let newest = syncs.iter().max_by_key(|sync| sync.attempted_ms);
    let status = newest.map_or(Status::Never, |sync| status_of(sync.health));
    let said = newest.map_or_else(String::new, |sync| words_of(sync.health, &sync.message));
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
        devices,
        enabled: account.enabled,
        incoming: mail_server(&account.config, "imap_host", "imap_port", 993),
        outgoing: mail_server(&account.config, "smtp_host", "smtp_port", 465),
        identities: identities_of(account),
        calendars: calendars_of(account),
        services,
        files_mode: account
            .config
            .get(accounts::cloud_types::FILES_MODE_KEY)
            .and_then(Value::as_str)
            .unwrap_or(if kind == Kind::Cloud { "sync" } else { "" })
            .to_owned(),
        paused: mounts
            .iter()
            .any(|mount| mount.account == account.id.as_str() && mount.paused),
        service: account.type_id.as_str().to_owned(),
        scope: account
            .config
            .get("messaging_scope")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

/// The addresses `account` sends as, besides its own.
fn identities_of(account: &accounts::Account) -> Vec<String> {
    account
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
        .unwrap_or_default()
}

/// The calendars `account` keeps, as the reading shows them.
fn calendars_of(account: &accounts::Account) -> Vec<CalendarView> {
    account
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
        .unwrap_or_default()
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

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod accounts_tests;
#[cfg(test)]
#[path = "mail_reach_tests.rs"]
mod mail_reach_tests;
