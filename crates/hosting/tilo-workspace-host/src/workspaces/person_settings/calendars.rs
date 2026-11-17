//! A person's calendars, kept by their holder.
//!
//! A calendar account is added the way a mail account is: its server is
//! asked first, with the name and password the person gives or the vault
//! item they name, and only a server that takes them is kept. The account
//! sits beside the mail accounts in the person's own store, its password in
//! their vault, in the shape a device's Calendar keeps one.
//!
//! The account's server stays the store. What the holder keeps beside the
//! account is a copy of what the server holds, entry by entry as the server
//! wrote it, with where each calendar's last pull ended. So the holder knows
//! every entry's time with every device off, and an entry is read, expanded
//! and filed by the Calendar app's own code ([`calendar::ics`],
//! [`calendar::spans`]): a device and its holder agree about time zones,
//! repeats and all-day entries because they are the same reader.
//!
//! Nothing here sends anything to anybody. An entry is written into the
//! person's own calendar; whoever it invites is mailed nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use calendar::account::{CalDavConfig, CalendarAccount, RemoteCalendar, TYPE_ID};
use calendar::model::Event;
use calendar::spans::{FreeAsked, SpanAsked};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_account_probe::{AccountTester as _, LiveAccountTester, local_http_allowed};
use tilo_actions::remote::person_vault::ItemRef;
use tilo_actions::remote::{ActionHostError as ActionError, Caller, InvokeRequest};
use tilo_dav::{Credentials, DavClient, Depth, Limits, Precondition, Url, props};
use tilo_user_identity::DeviceCert;

use super::{AskingAboutMail, NamedAgent, accounts_of_assistant, answer, argument, credentials};
use crate::workspaces::Workspaces;

/// The calendar server tests ask.
#[cfg(test)]
pub(crate) mod fake_caldav;

/// The resource of [`tilo_actions::remote::person_accounts::PROVIDER`] a
/// person's calendars are asked under.
pub const RESOURCE: &str = "calendars";
/// The calendar accounts the person keeps here, without their sign-ins.
pub const LIST: &str = "list";
/// Add an account with the password its person types. Theirs alone.
pub const ADD: &str = "add";
/// Add an account with the password a vault item keeps.
pub const ADD_WITH_ITEM: &str = "add-with-item";
/// Remove an account, its sign-in and what was kept of it. Theirs alone.
pub const REMOVE: &str = "remove";
/// Ask the servers now.
pub const SYNC: &str = "sync";
/// Every occurrence between two times.
pub const ENTRIES: &str = "entries";
/// The free time between two times.
pub const FREE: &str = "free";
/// Put the event an invitation describes into a calendar, once.
pub const ADD_INVITATION: &str = "add-invitation";
/// File an entry under categories, in its server's own field.
pub const SET_CATEGORIES: &str = "set-categories";

/// What an agent its person named for a calendar account may ask.
const ASSISTANT_MAY: [&str; 6] = [LIST, SYNC, ENTRIES, FREE, ADD_INVITATION, SET_CATEGORIES];
/// How old a copy may be before a read asks the server again.
const FRESH_MS: i64 = 60_000;
/// How old a copy may be before whoever waits on an entry's time asks the
/// server again: an entry moved on its server is known here within that.
const TIMES_FRESH_MS: i64 = 5 * 60_000;
const ENTRY_TYPE: &str = "text/calendar; charset=utf-8";
const MAX_ENTRY_BYTES: usize = 2 * 1024 * 1024;
const LIMITS: Limits = Limits {
    max_bytes: 8 * 1024 * 1024,
    max_responses: 10_000,
    max_depth: 32,
    max_nodes: 1_000_000,
    max_href_bytes: 4096,
};
const REPORTED: [tilo_dav::PropName; 2] = [props::GETETAG, props::CALENDAR_DATA];

/// One calendar of an account, as [`LIST`] answers it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CalendarView {
    id: String,
    name: String,
    read_only: bool,
}

/// One account, as [`LIST`] answers it: never its sign-in.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountView {
    id: String,
    name: String,
    server_url: String,
    username: String,
    has_secret: bool,
    calendars: Vec<CalendarView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    synced_at_ms: Option<i64>,
    /// Why the last pull did not end, in the server's or the network's words.
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct Listed {
    accounts: Vec<AccountView>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Add {
    server_url: String,
    username: String,
    password: String,
    #[serde(default)]
    name: Option<String>,
}

impl std::fmt::Debug for Add {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Add").finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddWithItem {
    server_url: String,
    #[serde(default)]
    username: Option<String>,
    password: ItemRef,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OneAccount {
    #[serde(default)]
    account: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Invitation {
    /// The calendar it goes into; the first the person may write when
    /// left out.
    #[serde(default)]
    calendar: Option<String>,
    ics: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Categories {
    entry: String,
    categories: Vec<String>,
}

/// An account as the store keeps it, read without its sign-in.
#[derive(Clone, Debug)]
struct Account {
    id: String,
    name: String,
    server_url: String,
    username: String,
    calendars: Vec<RemoteCalendar>,
}

#[derive(Deserialize)]
struct Stored {
    #[serde(default)]
    server_url: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    calendars: Vec<RemoteCalendar>,
}

/// What the holder keeps of an account's server.
#[derive(Default, Serialize, Deserialize)]
struct Kept {
    #[serde(default)]
    synced_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    problem: Option<String>,
    /// By the calendar's address.
    #[serde(default)]
    calendars: BTreeMap<String, KeptCalendar>,
}

#[derive(Default, Serialize, Deserialize)]
struct KeptCalendar {
    /// Where the last pull ended; none while the server gives no token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    /// By the entry's address.
    #[serde(default)]
    entries: BTreeMap<String, KeptEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct KeptEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    ics: String,
}

/// One event a server holds, and where.
struct Held {
    account: String,
    calendar: String,
    url: String,
    etag: Option<String>,
    ics: String,
    event: Event,
}

fn now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ns() / 1_000_000).unwrap_or(i64::MAX)
}

fn kept_at(root: &Path, account: &str) -> PathBuf {
    root.join("calendars").join(format!(
        "{}.json",
        blake3::hash(account.as_bytes()).to_hex()
    ))
}

/// What is kept of `account`. A copy that does not read starts over: the
/// next pull asks its server for everything.
fn read_kept(root: &Path, account: &str) -> Kept {
    let bytes = match std::fs::read(kept_at(root, account)) {
        Ok(bytes) => bytes,
        // Nothing was pulled yet.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Kept::default(),
        Err(error) => {
            tracing::warn!(%error, "calendars: what is kept of an account could not be read and starts over");
            return Kept::default();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(kept) => kept,
        Err(error) => {
            tracing::warn!(%error, "calendars: what is kept of an account does not read and starts over");
            Kept::default()
        }
    }
}

fn write_kept(root: &Path, account: &str, kept: &Kept) -> Result<(), String> {
    let bytes = serde_json::to_vec(kept).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &kept_at(root, account),
        &bytes,
        0o600,
    )
    .map_err(|error| error.to_string())
}

/// The calendar accounts kept under `root`.
fn accounts_at(root: &Path) -> Vec<Account> {
    accounts::AccountStore::load_in(root)
        .list(&accounts::AccountTypeId::from(TYPE_ID))
        .into_iter()
        .filter_map(|stored| {
            let config = stored.config_as::<Stored>()?;
            Some(Account {
                id: stored.id.to_string(),
                name: stored.display_name.clone(),
                server_url: config.server_url,
                username: config.username,
                calendars: config.calendars,
            })
        })
        .collect()
}

fn password_of(root: &Path, account: &str) -> Result<String, String> {
    credentials(root)?
        .get(&accounts::AccountId::from(account.to_owned()))
        .map(|secret| secret.password)
        .filter(|password| !password.is_empty())
        .ok_or_else(|| "That calendar account has no sign-in yet".to_owned())
}

fn view(root: &Path, account: &Account) -> AccountView {
    let kept = read_kept(root, &account.id);
    AccountView {
        id: account.id.clone(),
        name: account.name.clone(),
        server_url: account.server_url.clone(),
        username: account.username.clone(),
        has_secret: password_of(root, &account.id).is_ok(),
        calendars: account
            .calendars
            .iter()
            .map(|calendar| CalendarView {
                id: calendar.id.clone(),
                name: calendar.name.clone(),
                read_only: calendar.read_only,
            })
            .collect(),
        synced_at_ms: (kept.synced_at_ms > 0).then_some(kept.synced_at_ms),
        problem: kept.problem,
    }
}

/// The server's address, held to the one rule every calendar sign-in is
/// under: encrypted, or this machine itself in a development build, and
/// carrying no sign-in of its own.
fn server_of(server_url: &str) -> Result<Url, String> {
    let url = Url::parse(server_url.trim())
        .map_err(|_| "The calendar server's address does not read".to_owned())?;
    if url.scheme() != "https" && !local_http_allowed(&url) {
        return Err("refusing to send a credential without HTTPS".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("The server's address must not carry a sign-in".into());
    }
    Ok(url)
}

/// A connection that signs in to `account`'s server and follows no
/// redirect, so the sign-in reaches that server alone.
fn client(account: &Account, password: &str) -> Result<(DavClient, Url), String> {
    let server = server_of(&account.server_url)?;
    let client = DavClient::new(&tilo_dav::Options {
        user_agent: None,
        timeout: std::time::Duration::from_secs(20),
        connect_timeout: Some(std::time::Duration::from_secs(8)),
        redirects: tilo_dav::Redirects::Refuse,
    })
    .map_err(|_| "could not start the secure connection".to_owned())?
    .with_credentials(Credentials::new(
        account.username.clone(),
        password.to_owned(),
    ));
    Ok((client, server))
}

/// `address` when it is on `server`: a sign-in goes nowhere else.
fn on_server(server: &Url, address: &str) -> Result<Url, String> {
    Url::parse(address)
        .ok()
        .filter(|url| url.origin() == server.origin())
        .ok_or_else(|| "a calendar's address is on its account's server".to_owned())
}

/// Pull every calendar of `account` into `kept`: what changed since the
/// last pull where the server tells, everything where it does not.
fn pull(account: &Account, password: &str, kept: &mut Kept) -> Result<(), String> {
    let (client, server) = client(account, password)?;
    let wanted: BTreeSet<&str> = account
        .calendars
        .iter()
        .map(|one| one.id.as_str())
        .collect();
    kept.calendars.retain(|id, _| wanted.contains(id.as_str()));
    for calendar in &account.calendars {
        let collection = on_server(&server, &calendar.id)?;
        let state = kept.calendars.entry(calendar.id.clone()).or_default();
        let asked = tilo_dav::body::sync_collection_body(
            state.token.as_deref().unwrap_or_default(),
            &REPORTED,
        );
        let said =
            |error: tilo_dav::client::Error| format!("the calendar server did not answer: {error}");
        let mut response = client
            .report(collection.clone(), Depth::One, asked)
            .map_err(said)?;
        let mut whole = state.token.is_none();
        let mut tells = true;
        // A token the server no longer knows, and a server that never
        // learned to tell what changed: read everything it holds.
        if matches!(response.status().as_u16(), 400 | 403 | 405 | 409 | 501) {
            (whole, tells) = (true, false);
            let everything = tilo_dav::body::calendar_query_body(&REPORTED, None);
            response = client
                .report(collection.clone(), Depth::One, everything)
                .map_err(said)?;
        }
        if !response.status().is_success() {
            return Err(format!(
                "the calendar server answered {}",
                response.status().as_u16()
            ));
        }
        let listing = response
            .multistatus(&LIMITS)
            .map_err(|error| format!("the calendar server's answer does not read: {error}"))?;
        let token = listing.sync_token.clone().filter(|_| tells);
        if whole {
            state.entries.clear();
        }
        for found in listing.responses {
            let Ok(url) = collection.join(&found.href) else {
                continue;
            };
            if url.origin() != server.origin() {
                continue;
            }
            if found.status == Some(404) {
                state.entries.remove(url.as_str());
                continue;
            }
            let Some(ics) = found
                .prop("calendar-data")
                .and_then(|data| data.text.as_deref())
            else {
                continue;
            };
            if ics.len() > MAX_ENTRY_BYTES {
                return Err("an entry of the calendar is larger than 2 MiB".into());
            }
            state.entries.insert(
                url.to_string(),
                KeptEntry {
                    etag: found.prop_text("getetag").map(str::to_owned),
                    ics: ics.to_owned(),
                },
            );
        }
        state.token = token;
    }
    Ok(())
}

/// Ask `account`'s server now and keep what it answers. A pull that does
/// not end leaves the copy as it was and says why with it.
fn sync(root: &Path, account: &Account) -> Result<(), String> {
    let mut kept = read_kept(root, &account.id);
    let pulled =
        password_of(root, &account.id).and_then(|password| pull(account, &password, &mut kept));
    match &pulled {
        Ok(()) => {
            kept.synced_at_ms = now_ms();
            kept.problem = None;
        }
        Err(problem) => {
            // The copy as it stood: half a pull is not kept.
            kept = read_kept(root, &account.id);
            kept.problem = Some(problem.clone());
        }
    }
    write_kept(root, &account.id, &kept)?;
    pulled
}

/// Every event the servers of `accounts` hold, as last pulled.
fn held(root: &Path, accounts: &[Account]) -> Vec<Held> {
    let mut all = Vec::new();
    for account in accounts {
        for (calendar, state) in read_kept(root, &account.id).calendars {
            for (url, entry) in state.entries {
                for mut event in calendar::ics::parse(&entry.ics).0 {
                    event.calendar_id = Some(calendar.clone());
                    all.push(Held {
                        account: account.id.clone(),
                        calendar: calendar.clone(),
                        url: url.clone(),
                        etag: entry.etag.clone(),
                        ics: entry.ics.clone(),
                        event,
                    });
                }
            }
        }
    }
    all
}

/// The id an account is kept under: its name at its server, which no mail
/// account's address is.
fn account_id(username: &str, server: &Url) -> String {
    // The address carries no sign-in ([`server_of`]), so its authority is
    // the server's name, with the port where that is not https's own.
    format!("caldav:{username}@{}", server.authority())
}

/// Add an account once its server answered the sign-in with the calendars
/// it holds: the password into the vault, the account into the store.
fn add(root: &Path, adding: Add) -> Result<AccountView, String> {
    let username = adding.username.trim().to_owned();
    if username.is_empty() || adding.password.is_empty() {
        return Err("A calendar account has a user name and a password".into());
    }
    let server = server_of(&adding.server_url)?;
    let id = account_id(&username, &server);
    let mut asked = accounts::Account::new(accounts::AccountId::from(id.clone()), TYPE_ID);
    asked.config = serde_json::json!({ "server_url": server.as_str(), "username": username });
    let found = LiveAccountTester
        .discover_calendars(&asked, &accounts::Secret::password(adding.password.clone()))?;
    let calendars = found
        .into_iter()
        .enumerate()
        .map(|(place, calendar)| RemoteCalendar {
            id: calendar.href,
            name: calendar.name,
            color: u8::try_from(place % 7).unwrap_or_default(),
            visible: true,
            read_only: calendar.read_only,
        })
        .collect();
    let mut store = calendar::AccountStore::new();
    store.upsert(CalendarAccount {
        id: id.clone(),
        config: CalDavConfig {
            display_name: adding
                .name
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| username.clone()),
            server_url: server.to_string(),
            username,
            password: adding.password,
            oauth_account_id: None,
        },
        calendars,
        collapsed: false,
    });
    store.persist_in(root).map_err(|error| error.to_string())?;
    let account = accounts_at(root)
        .into_iter()
        .find(|account| account.id == id)
        .ok_or_else(|| "the account was kept but did not read back".to_owned())?;
    // What it holds now; a server that is slow to answer is asked again
    // by the first read.
    let _ = sync(root, &account);
    Ok(view(root, &account))
}

/// Add an account with the sign-in the vault item `adding` names keeps:
/// a login that belongs to the server's own origin, and to nothing else.
fn add_with_item(root: &Path, adding: AddWithItem, by: &str) -> Result<AccountView, String> {
    let server = server_of(&adding.server_url)?;
    let place =
        accounts::vault_items::Place::parse(server.as_str()).map_err(|error| error.to_string())?;
    let mut sign_in = crate::workspaces::person_vault::released(
        root,
        &adding.password,
        accounts::vault_items::ItemKind::Login,
        std::slice::from_ref(&place),
        by,
    )?;
    let username = adding
        .username
        .filter(|username| !username.trim().is_empty())
        .unwrap_or_else(|| std::mem::take(&mut sign_in.username));
    add(
        root,
        Add {
            server_url: adding.server_url,
            username,
            password: sign_in.secret.to_string(),
            name: adding.name,
        },
    )
}

fn remove(root: &Path, account: &str) -> Result<Listed, String> {
    if !accounts_at(root).iter().any(|kept| kept.id == account) {
        return Err("That calendar account is not kept here".into());
    }
    // The account, its sign-in and who was named for it go together.
    super::remove_mail(root, account)?;
    let _ = std::fs::remove_file(kept_at(root, account));
    Ok(Listed {
        accounts: accounts_at(root)
            .iter()
            .map(|account| view(root, account))
            .collect(),
    })
}

/// Bring the copies of `accounts` up to date when they are older than a
/// reader should be answered from. A server that does not answer leaves
/// its copy, with why.
fn freshen(root: &Path, accounts: &[Account], fresh_ms: i64) {
    for account in accounts {
        if now_ms().saturating_sub(read_kept(root, &account.id).synced_at_ms) > fresh_ms {
            let _ = sync(root, account);
        }
    }
}

fn entries(
    root: &Path,
    accounts: &[Account],
    asked: &SpanAsked,
    fresh_ms: i64,
) -> Result<Value, String> {
    freshen(root, accounts, fresh_ms);
    let held = held(root, accounts);
    let events: Vec<&Event> = held.iter().map(|held| &held.event).collect();
    let mut answered = asked.answer(&events)?;
    for occurrence in &mut answered.entries {
        if let Some(of) = held
            .iter()
            .find(|held| held.calendar == occurrence.calendar)
        {
            occurrence.account.clone_from(&of.account);
        }
    }
    let mut value = serde_json::to_value(&answered).map_err(|error| error.to_string())?;
    if let Some(map) = value.as_object_mut() {
        let synced = accounts
            .iter()
            .map(|account| read_kept(root, &account.id).synced_at_ms)
            .min()
            .unwrap_or_default();
        map.insert("syncedAtMs".into(), synced.into());
    }
    Ok(value)
}

fn free(root: &Path, accounts: &[Account], asked: &FreeAsked) -> Result<Value, String> {
    freshen(root, accounts, FRESH_MS);
    let held = held(root, accounts);
    let events: Vec<&Event> = held.iter().map(|held| &held.event).collect();
    serde_json::to_value(asked.answer(&events)?).map_err(|error| error.to_string())
}

/// Write `ics` to `url` on `account`'s server under `precondition`.
/// `Ok(false)` when the server holds something else there than expected.
fn write(
    root: &Path,
    account: &Account,
    url: &str,
    ics: String,
    precondition: Precondition<'_>,
) -> Result<bool, String> {
    let (client, server) = client(account, &password_of(root, &account.id)?)?;
    let response = client
        .put(
            on_server(&server, url)?,
            ics.into_bytes(),
            Some(ENTRY_TYPE),
            precondition,
        )
        .map_err(|error| format!("the calendar server did not answer: {error}"))?;
    match response.status().as_u16() {
        412 => Ok(false),
        status if response.status().is_success() => {
            let _ = status;
            Ok(true)
        }
        403 => Err("That calendar is read-only".into()),
        status => Err(format!("the calendar server answered {status}")),
    }
}

/// The calendar an invitation goes into: the one named, else the first
/// the person may write.
fn writable<'a>(
    accounts: &'a [Account],
    named: Option<&str>,
) -> Result<(&'a Account, &'a RemoteCalendar), String> {
    let mut all = accounts
        .iter()
        .flat_map(|account| account.calendars.iter().map(move |one| (account, one)));
    let found = match named {
        Some(named) => all.find(|(_, calendar)| calendar.id == named || calendar.name == named),
        None => all.find(|(_, calendar)| !calendar.read_only),
    }
    .ok_or_else(|| match named {
        Some(_) => "That calendar is not one of the person's".to_owned(),
        None => "The person keeps no calendar that can be written".to_owned(),
    })?;
    if found.1.read_only {
        return Err("That calendar is read-only".into());
    }
    Ok(found)
}

/// Put the event `invitation` describes into a calendar: created under the
/// invitation's own identity, so the same invitation again finds it.
fn add_invitation(
    root: &Path,
    accounts: &[Account],
    invitation: &Invitation,
) -> Result<Value, String> {
    let calendar::ics::Invitation { event, cancels } = calendar::ics::invitation(&invitation.ics)?;
    let (account, calendar) = writable(accounts, invitation.calendar.as_deref())?;
    let _ = sync(root, account);
    let said = |made: &str| serde_json::json!({ "entry": event.id.to_string(), "made": made });
    let existing = held(root, std::slice::from_ref(account))
        .into_iter()
        .find(|held| held.event.id == event.id);
    let text = calendar::ics::kept_text(&invitation.ics);
    let made = match (existing, cancels) {
        (None, true) => "unchanged",
        (Some(held), true) => {
            let (client, server) = client(account, &password_of(root, &account.id)?)?;
            let matching = held.etag.as_deref().unwrap_or("*");
            let response = client
                .delete(
                    on_server(&server, &held.url)?,
                    Precondition::IfMatch(matching),
                )
                .map_err(|error| format!("the calendar server did not answer: {error}"))?;
            if !response.status().is_success() && response.status().as_u16() != 404 {
                return Err(format!(
                    "the calendar server answered {}",
                    response.status().as_u16()
                ));
            }
            "cancelled"
        }
        (Some(held), false) => {
            let same = calendar::ics::parse(&text)
                .0
                .into_iter()
                .next()
                .is_some_and(|mut sent| {
                    sent.calendar_id.clone_from(&held.event.calendar_id);
                    sent == held.event
                });
            if same {
                "unchanged"
            } else {
                // What its person filed it under on the server stays.
                let kept: Vec<String> = held.event.categories.iter().cloned().collect();
                let text = if event.categories.is_empty() && !kept.is_empty() {
                    calendar::ics::with_categories(&text, &kept).unwrap_or(text)
                } else {
                    text
                };
                let matching = held.etag.as_deref().unwrap_or("*");
                if !write(
                    root,
                    account,
                    &held.url,
                    text,
                    Precondition::IfMatch(matching),
                )? {
                    return Err("The entry changed on its server meanwhile: ask again".into());
                }
                "updated"
            }
        }
        (None, false) => {
            let url = format!("{}/{}.ics", calendar.id.trim_end_matches('/'), event.id);
            if !write(root, account, &url, text, Precondition::IfNoneMatchAny)? {
                return Err("The entry changed on its server meanwhile: ask again".into());
            }
            "created"
        }
    };
    if made != "unchanged" {
        let _ = sync(root, account);
    }
    Ok(said(made))
}

/// File the entry `asked` names under its categories, in the text its
/// server holds: every other line of the entry stays as the server has it.
fn set_categories(root: &Path, accounts: &[Account], asked: &Categories) -> Result<Value, String> {
    if asked.categories.len() > calendar::model::MAX_CATEGORIES {
        return Err("an entry has at most 32 categories".into());
    }
    let categories = asked
        .categories
        .iter()
        .map(|named| calendar::model::category(named))
        .collect::<Option<Vec<String>>>()
        .ok_or("a category is a few words of at most 64 characters")?;
    freshen(root, accounts, FRESH_MS);
    let held = held(root, accounts)
        .into_iter()
        .find(|held| held.event.id.to_string() == asked.entry)
        .ok_or("That entry is in none of the person's calendars")?;
    let account = accounts
        .iter()
        .find(|account| account.id == held.account)
        .ok_or("That entry is in none of the person's calendars")?;
    let text = calendar::ics::with_categories(&held.ics, &categories)
        .ok_or("the entry as its server holds it does not read")?;
    let matching = held.etag.as_deref().unwrap_or("*");
    if !write(
        root,
        account,
        &held.url,
        text,
        Precondition::IfMatch(matching),
    )? {
        return Err("The entry changed on its server meanwhile: ask again".into());
    }
    let _ = sync(root, account);
    Ok(serde_json::json!({ "entry": asked.entry, "categories": categories }))
}

/// Do `work`, which asks servers, off the threads that answer requests.
async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| "the calendar server was not asked to the end".to_owned())?
}

impl Workspaces {
    /// Whether `caller` is an agent its person named for at least one of
    /// their calendar accounts: what the catalogue offers the calendar
    /// actions by.
    pub(in crate::workspaces) fn reads_calendars_as_assistant(
        &self,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<collab_types::WorkspaceId>,
    ) -> bool {
        match Self::asking_about_mail(caller, certificate, workspace_id) {
            Ok(AskingAboutMail::Assistant {
                person,
                agent,
                workspace,
            }) => {
                let root = self.person_settings_root(&person);
                let named = accounts_of_assistant(
                    &root,
                    NamedAgent {
                        workspace: &workspace,
                        agent: &agent,
                    },
                );
                accounts_at(&root)
                    .iter()
                    .any(|account| named.contains(&account.id))
            }
            _ => false,
        }
    }

    /// Every occurrence between `from_ms` and `to_ms` in the calendar
    /// accounts `person` named the agent `agent` of `workspace` for, each as
    /// [`ENTRIES`] answers one: what an automation that waits on an entry's
    /// time reads before it tells that agent. Nothing where they named it
    /// for no calendar account, so an automation hands an agent nothing of a
    /// calendar its person did not let it read. A copy older than five
    /// minutes is brought up to date first, and a server that does not
    /// answer leaves the copy as it stood.
    ///
    /// # Errors
    /// The span does not read, or the read did not end.
    pub async fn calendar_occurrences_for(
        &self,
        person: &[u8; 32],
        workspace: &str,
        agent: &str,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<Value>, String> {
        let root = self.person_settings_root(person);
        let named = accounts_of_assistant(&root, NamedAgent { workspace, agent });
        let mut accounts = accounts_at(&root);
        accounts.retain(|account| named.contains(&account.id));
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let asked = SpanAsked::between_ms(from_ms, to_ms)
            .ok_or_else(|| "a time no calendar reaches".to_owned())?;
        off_thread(move || {
            let mut read = entries(&root, &accounts, &asked, TIMES_FRESH_MS)?;
            match read.get_mut("entries").map(Value::take) {
                Some(Value::Array(occurrences)) => Ok(occurrences),
                _ => Ok(Vec::new()),
            }
        })
        .await
    }

    /// Add a calendar account with the sign-in a vault item keeps. A
    /// sign-in named by reference carries none, so the person or an agent
    /// acting for them may ask: the vault decides who uses it and where.
    async fn calendar_by_reference(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionError> {
        let (person, by) = Self::asking_for_person(caller, certificate, workspace_id)?;
        let root = self.person_settings_root(&person);
        let adding: AddWithItem = argument(
            request,
            "a calendar account names its server and the vault item that is its password",
        )?;
        let kept = off_thread(move || add_with_item(&root, adding, &by)).await;
        if kept.is_ok() {
            self.person_settings_changed();
        }
        answer(&kept)
    }

    /// Whose calendars `caller` asks about with `action`, and which of
    /// their accounts it may ask of: every one for the person, the ones it
    /// was named for for an agent of theirs, which reads and files and
    /// removes nothing. Unknown and forbidden are the same answer.
    fn calendars_asked(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        action: &str,
    ) -> Result<(PathBuf, Vec<Account>), ActionError> {
        match Self::asking_about_mail(caller, certificate, workspace_id)? {
            AskingAboutMail::Person(person) => {
                let root = self.person_settings_root(&person);
                let accounts = accounts_at(&root);
                Ok((root, accounts))
            }
            AskingAboutMail::Assistant {
                person,
                agent,
                workspace,
            } => {
                let root = self.person_settings_root(&person);
                let named = accounts_of_assistant(
                    &root,
                    NamedAgent {
                        workspace: &workspace,
                        agent: &agent,
                    },
                );
                let mut accounts = accounts_at(&root);
                accounts.retain(|account| named.contains(&account.id));
                if accounts.is_empty() || !ASSISTANT_MAY.contains(&action) {
                    return Err(ActionError::UnknownAction);
                }
                Ok((root, accounts))
            }
        }
    }

    /// Answer a request about a person's calendars ([`RESOURCE`]): the
    /// person on a device of theirs, or an agent they named for the
    /// account.
    pub(super) async fn person_calendars_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionError> {
        let action = request.target.address.action.clone();
        if action == ADD_WITH_ITEM {
            return self
                .calendar_by_reference(workspace_id, caller, certificate, request)
                .await;
        }
        let (root, accounts) = self.calendars_asked(workspace_id, caller, certificate, &action)?;
        let listed = |root: &Path, accounts: &[Account]| Listed {
            accounts: accounts.iter().map(|account| view(root, account)).collect(),
        };
        match action.as_str() {
            LIST => answer(&Ok::<_, String>(listed(&root, &accounts))),
            ADD => {
                let adding: Add = argument(request, "a calendar account to add")?;
                let kept = off_thread(move || add(&root, adding)).await;
                if kept.is_ok() {
                    self.person_settings_changed();
                }
                answer(&kept)
            }
            REMOVE => {
                let asked: OneAccount = argument(request, "the account")?;
                let kept = remove(&root, asked.account.as_deref().unwrap_or_default());
                if kept.is_ok() {
                    self.person_settings_changed();
                }
                answer(&kept)
            }
            SYNC => {
                let asked: OneAccount = match &request.argument {
                    tilo_actions::remote::InvokeArgument::Json(_) => {
                        argument(request, "the account")?
                    }
                    _ => OneAccount::default(),
                };
                let pulled = off_thread(move || {
                    for account in accounts
                        .iter()
                        .filter(|account| asked.account.as_ref().is_none_or(|id| *id == account.id))
                    {
                        sync(&root, account)?;
                    }
                    Ok(listed(&root, &accounts))
                });
                answer(&pulled.await)
            }
            ENTRIES => {
                let asked: SpanAsked = argument(request, "a span to read")?;
                answer(&off_thread(move || entries(&root, &accounts, &asked, FRESH_MS)).await)
            }
            FREE => {
                let asked: FreeAsked = argument(request, "a span to find free time in")?;
                answer(&off_thread(move || free(&root, &accounts, &asked)).await)
            }
            ADD_INVITATION => {
                let asked: Invitation = argument(request, "an invitation to add")?;
                answer(&off_thread(move || add_invitation(&root, &accounts, &asked)).await)
            }
            SET_CATEGORIES => {
                let asked: Categories = argument(request, "an entry and its categories")?;
                answer(&off_thread(move || set_categories(&root, &accounts, &asked)).await)
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}

/// What a test of something that reads a person's calendars keeps of them
/// without a calendar server at hand.
#[cfg(any(test, feature = "test-support"))]
impl Workspaces {
    /// Keep a calendar account for `person` as if its server had been pulled
    /// this moment: one calendar that holds `entries`, each the iCalendar
    /// text of one entry, with the agent `named` (a workspace and the
    /// agent's id there) named for the account when one is given. Answers
    /// the account's id. Calling it again replaces what the calendar holds,
    /// as the next pull does after another client changed it.
    ///
    /// # Errors
    /// The account or the copy could not be kept.
    pub fn keep_calendar_as_pulled(
        &self,
        person: &[u8; 32],
        named: Option<(&str, &str)>,
        entries: &[String],
    ) -> Result<String, String> {
        const SERVER: &str = "https://calendar.example.test/dav/";
        const CALENDAR: &str = "https://calendar.example.test/dav/calendars/ada/work/";
        let root = self.person_settings_root(person);
        let id = account_id("ada", &server_of(SERVER)?);
        let mut store = calendar::AccountStore::new();
        store.upsert(CalendarAccount {
            id: id.clone(),
            config: CalDavConfig {
                display_name: "Work".into(),
                server_url: SERVER.into(),
                username: "ada".into(),
                password: "kept-for-a-test".into(),
                oauth_account_id: None,
            },
            calendars: vec![RemoteCalendar {
                id: CALENDAR.into(),
                name: "Work".into(),
                color: 0,
                visible: true,
                read_only: false,
            }],
            collapsed: false,
        });
        store.persist_in(&root).map_err(|error| error.to_string())?;
        if let Some((workspace, agent)) = named {
            let naming = tilo_actions::remote::person_accounts::AssistantAccess {
                account: id.clone(),
                agent: agent.to_owned(),
                may: true,
            };
            super::say_assistant(&root, workspace, &naming)?;
        }
        let mut kept = Kept {
            synced_at_ms: now_ms(),
            ..Kept::default()
        };
        let calendar = kept.calendars.entry(CALENDAR.to_owned()).or_default();
        for (place, ics) in entries.iter().enumerate() {
            calendar.entries.insert(
                format!("{CALENDAR}{place}.ics"),
                KeptEntry {
                    etag: None,
                    ics: ics.clone(),
                },
            );
        }
        write_kept(&root, &id, &kept)?;
        Ok(id)
    }
}
