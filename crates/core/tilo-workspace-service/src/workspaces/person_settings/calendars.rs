//! A person's calendars, kept by their holder.
//!
//! A calendar account is added the way a mail account is: its server is
//! asked first, with the name and password the person gives or the vault
//! item they name, and only a server that takes them is kept. The account
//! sits beside the mail accounts in the workspace's store, in the shape a
//! device's Calendar keeps one, and names its sign-in: an entry of the
//! workspace's vault, resolved each time its server is asked
//! ([`super::kept_sign_ins`]).
//!
//! An account may be connected for reading only ([`Add`]). Its settings say
//! so in that same store, where a device's Calendar reads it too, and every
//! calendar of it is kept as one that cannot be written. Nothing is ever
//! written to such an account's server from here: not for its person, not
//! for an agent they named. Each asking is refused in words that say so
//! ([`READ_ONLY`]), before anybody is asked anything. It stays so until its
//! person removes the account.
//!
//! The account's server stays the store. What the holder keeps beside the
//! account is a copy of what the server holds, entry by entry as the server
//! wrote it, with where each calendar's last pull ended. So the holder knows
//! every entry's time with every device off, and an entry is read, expanded
//! and filed by the calendar origin's own reader ([`tilolang_calendar::ics`],
//! [`tilolang_calendar::spans`]): a device and its holder agree about time zones,
//! repeats and all-day entries because they are the same reader.
//!
//! Nothing here sends anything to anybody, and nothing here starts a
//! calendar server doing it: an entry is written into the person's own
//! calendar with every line that names somebody saying that the server is
//! not the one to write to them (`SCHEDULE-AGENT=CLIENT`, RFC 6638), and it
//! is removed with `Schedule-Reply: F`. Whoever it invites is mailed
//! nothing. What a server already does is not undone either: a line it
//! writes for, in an entry another client stored, goes up as the server
//! holds it, because marking it then would have the server tell that person
//! the event is off (RFC 6638, 3.2.1.2; [`tilolang_calendar::ics::kept_beside`]).
//!
//! The person changes their calendar by asking. An agent they named for the
//! account hands over text of its own writing, so what it may do with it is
//! narrower ([`judged`]). Unasked it puts in an invitation to its person:
//! somebody else organises it, and an address of the person's is among those
//! it invites. It changes or removes only such an entry that it put in
//! itself ([`PutIn`]), for that same organiser: the organiser its text names
//! proves nothing, so an invitation that reached the calendar another way is
//! its person's own, as every other entry the calendar holds is.
//! Text that names people otherwise (its person as the organiser, or people
//! and not its person) is put in only once the person was shown every
//! address it names and approved the call, or where a grant of theirs lets
//! that agent do so. Filing an entry under categories
//! rewrites it, so the same holds there ([`filed`]): unasked the agent files
//! what it put in itself, what is its person's own it files once they
//! allowed it, and an entry whose lines leave writing to its server it does
//! not rewrite at all.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};

use accounts::calendar::{RemoteCalendar, TYPE_ID};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_account_probe::imap::Reach;
use tilo_account_probe::{AccountTester as _, FindingAccountTester};
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::person_accounts;
use tilo_actions::remote::person_vault::ItemRef;
use tilo_actions::remote::{ActionCallError as ActionError, Caller, InvokeRequest};
use tilo_dav::{Credentials, DavClient, Depth, Limits, Precondition, Url, props};
use tilo_user_identity::DeviceCert;
use tilolang_calendar::model::Event;
use tilolang_calendar::spans::{FreeAsked, SpanAsked};

use super::kept_sign_ins::{NO_SIGN_IN, SIGN_IN, Signing, places_of, sign_in_of};
use super::{answer, argument, mail_accounts, sending};
use crate::workspaces::Workspaces;
use crate::workspaces::vault::{VaultAt, VaultOf};
use crate::workspaces::workspace_stores::Store;

/// The calendar server tests ask.
#[cfg(test)]
pub(crate) mod fake_caldav;

/// What a turn this server carries out reads of them.
pub(in crate::workspaces) mod turn;

/// A calendar connected by a turn's telling: asked for, and carried out.
pub(in crate::workspaces) mod connect;

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
/// Connect an account by a turn's telling, for reading only ([`connect`]).
pub const CONNECT: &str = "connect";

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
/// How much of an entry's title a question shows before it says that more
/// follows.
const TITLE_CHARS: usize = 200;
/// The most a read answers of an entry's notes, in characters: what an
/// entry is about fits, and a long text pasted into one does not come along.
const NOTES_CHARS: usize = 2_000;
/// What an agent is told of an entry that is its person's own.
const THEIRS: &str = "The calendar holds an entry under this invitation's identity that did not \
                      come from its sender. An entry its person entered themselves, or invites \
                      others to, is theirs to change or remove: tell them what the invitation \
                      says.";
/// What an agent is told of an invitation it did not put in itself.
const ANOTHER_WAY: &str = "The calendar holds this invitation already, and this agent did not \
                           put it there. An entry that reached its person's calendar another \
                           way (their calendar server filed it, or they did) is theirs to \
                           change or remove: tell them what the invitation says.";
/// What an agent is told of an entry it would file whose lines leave writing
/// to the people it names to its calendar server.
const SERVER_WRITES: &str = "This entry names people, and its lines leave writing to them to \
                             its calendar server, which may write to them when the entry is \
                             rewritten. Filing it is its person's to do where they keep their \
                             calendar: tell them what you would file it under.";

/// What every writing to the calendar server of an account connected for
/// reading only is refused with.
const READ_ONLY: &str = "This calendar account is connected for reading only, so nothing is \
                         written to its calendar server.";

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
    /// Whether the account is connected for reading only: nothing is ever
    /// written to its calendar server.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    read_only: bool,
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
    /// Connect it for reading only: nothing is ever written to its server.
    #[serde(default)]
    read_only: bool,
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
    /// Connect it for reading only: nothing is ever written to its server.
    #[serde(default)]
    read_only: bool,
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

/// A calendar account as the workspace's accounts keep it, read without its sign-in.
#[derive(Clone, Debug)]
struct Account {
    /// The account as kept, which names its sign-in and its server.
    stored: accounts::Account,
    id: String,
    name: String,
    server_url: String,
    username: String,
    calendars: Vec<RemoteCalendar>,
    /// Whether it is connected for reading only.
    read_only: bool,
}

#[derive(Deserialize)]
struct Stored {
    #[serde(default)]
    server_url: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    calendars: Vec<RemoteCalendar>,
    #[serde(default)]
    read_only: bool,
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
    /// The agent that put an entry in, by the entry's address, until a pull
    /// finds the entry gone.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    put_in: BTreeMap<String, PutIn>,
}

/// The agent that put an entry into a calendar: the one whose it is to bring
/// up to date, to take out again and to file unasked. Kept by this holder
/// beside its copy of the calendar, where no text an agent hands over
/// reaches it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PutIn {
    workspace: String,
    agent: String,
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
    /// The agent that put it in, where one did.
    put_in: Option<PutIn>,
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

/// The calendar accounts of the workspace's accounts `held`.
fn accounts_in(held: &accounts::person::AccountList) -> Vec<Account> {
    held.of_type(TYPE_ID)
        .into_iter()
        .filter_map(|stored| {
            let mut config = stored.config_as::<Stored>()?;
            let stored = &stored;
            // The account's word decides: every calendar of an account
            // connected for reading only is one that cannot be written,
            // whatever was kept of what its server lets the person do.
            for calendar in &mut config.calendars {
                calendar.read_only |= config.read_only;
            }
            Some(Account {
                stored: stored.clone(),
                id: stored.id.to_string(),
                name: stored.display_name.clone(),
                server_url: config.server_url,
                username: config.username,
                calendars: config.calendars,
                read_only: config.read_only,
            })
        })
        .collect()
}

/// What signs in to `account`'s server now, for whoever `signing` signs in for: the password
/// its entry in the workspace's vault holds, at its server's own origin. The use is written
/// into the entry first.
async fn signed_in(signing: Signing<'_>, account: &Account) -> Result<Credentials, String> {
    let reference = sign_in_of(&account.stored.config).ok_or_else(|| NO_SIGN_IN.to_owned())?;
    let places = places_of(&account.stored)?;
    let released = signing
        .vault
        .use_sign_in(
            &reference,
            (accounts::vault_items::ItemKind::Login, &places),
            signing.by,
            SIGN_IN,
        )
        .await?;
    Ok(Credentials::new(
        account.username.clone(),
        released.secret.to_string(),
    ))
}

/// What asks a workspace's calendar servers, from the thread that does: how far it may reach,
/// the workspace's vault every sign-in is resolved from the moment it signs in, and who signs
/// in (empty for a person or this host itself), which each use writes down.
#[derive(Clone)]
pub(super) struct Asking {
    reach: Reach,
    vault: VaultOf,
    by: String,
    runtime: tokio::runtime::Handle,
}

impl Asking {
    /// The workspace's accounts now, read on the runtime the sign-ins are resolved on.
    fn accounts(&self) -> Result<accounts::person::AccountList, String> {
        self.runtime.block_on(self.vault.at().accounts())
    }

    /// The workspace's calendar accounts now.
    fn calendar_accounts(&self) -> Result<Vec<Account>, String> {
        self.accounts().map(|held| accounts_in(&held))
    }

    /// Asking within `reach`, signing in from `vault` for `by`. Made on the runtime the sign-ins
    /// are resolved on.
    pub(super) fn of(vault: VaultAt<'_>, by: &str, reach: Reach) -> Self {
        Self {
            reach,
            vault: vault.owned(),
            by: by.to_owned(),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    fn signing<'a>(&'a self, root: &'a Path) -> Signing<'a> {
        Signing {
            vault: self.vault.at(),
            root,
            by: &self.by,
        }
    }

    /// What signs in to `account`'s server now ([`signed_in`]).
    fn sign_in(&self, root: &Path, account: &Account) -> Result<Credentials, String> {
        self.runtime
            .block_on(signed_in(self.signing(root), account))
    }
}

fn view(root: &Path, account: &Account) -> AccountView {
    let kept = read_kept(root, &account.id);
    AccountView {
        id: account.id.clone(),
        name: account.name.clone(),
        server_url: account.server_url.clone(),
        username: account.username.clone(),
        has_secret: super::kept_sign_ins::has_sign_in(&account.stored),
        calendars: account
            .calendars
            .iter()
            .map(|calendar| CalendarView {
                id: calendar.id.clone(),
                name: calendar.name.clone(),
                read_only: calendar.read_only,
            })
            .collect(),
        read_only: account.read_only,
        synced_at_ms: (kept.synced_at_ms > 0).then_some(kept.synced_at_ms),
        problem: kept.problem,
    }
}

/// The server's address, held to the one rule every calendar sign-in is
/// under ([`accounts::is_secure_endpoint`]): encrypted, or this machine
/// itself, and carrying no sign-in of its own.
fn server_of(server_url: &str) -> Result<Url, String> {
    let url = Url::parse(server_url.trim())
        .map_err(|_| "The calendar server's address does not read".to_owned())?;
    if !accounts::is_secure_endpoint(url.as_str()) {
        return Err("refusing to send a credential without HTTPS".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("The server's address must not carry a sign-in".into());
    }
    Ok(url)
}

/// A connection that signs in to `account`'s server and follows no
/// redirect, so the sign-in reaches that server alone. It goes only where
/// `reach` lets it, judged at the connection itself: what the server's name
/// answered when the account was added decides nothing now.
fn client(
    account: &Account,
    credentials: Credentials,
    reach: &Reach,
) -> Result<(DavClient, Url), String> {
    let server = server_of(&account.server_url)?;
    let client = DavClient::new(&tilo_dav::Options {
        user_agent: None,
        timeout: std::time::Duration::from_secs(20),
        connect_timeout: Some(std::time::Duration::from_secs(8)),
        redirects: tilo_dav::Redirects::Refuse,
        lookup: Some(reach.lookup()),
    })
    .map_err(|_| "could not start the secure connection".to_owned())?
    .with_credentials(credentials);
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
fn pull(
    account: &Account,
    credentials: Credentials,
    kept: &mut Kept,
    reach: &Reach,
) -> Result<(), String> {
    let (client, server) = client(account, credentials, reach)?;
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
        // What an agent put in and is gone is nobody's any more.
        let KeptCalendar {
            entries, put_in, ..
        } = state;
        put_in.retain(|address, _| entries.contains_key(address));
    }
    Ok(())
}

/// Ask `account`'s server now and keep what it answers. A pull that does
/// not end leaves the copy as it was and says why with it.
fn sync(root: &Path, reach: &Asking, account: &Account) -> Result<(), String> {
    let mut kept = read_kept(root, &account.id);
    let pulled = reach
        .sign_in(root, account)
        .and_then(|credentials| pull(account, credentials, &mut kept, &reach.reach));
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
                for mut event in tilolang_calendar::ics::parse(&entry.ics).0 {
                    event.calendar_id = Some(calendar.clone());
                    all.push(Held {
                        account: account.id.clone(),
                        calendar: calendar.clone(),
                        url: url.clone(),
                        etag: entry.etag.clone(),
                        ics: entry.ics.clone(),
                        put_in: state.put_in.get(&url).cloned(),
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
/// it holds: the account into the store, naming its sign-in in the
/// workspace's vault ([`keep_account`]), `named` where a member's entry is
/// it. Its server is asked within `reach`, as it is at every later read.
fn add(
    root: &Path,
    reach: &Asking,
    adding: Add,
    named: Option<ItemRef>,
    mail_account: Option<&str>,
) -> Result<AccountView, String> {
    let stored = prepared(reach, adding, named, mail_account)?;
    let id = stored.id.to_string();
    reach
        .runtime
        .block_on(reach.vault.at().keep_account(&stored))?;
    let account = reach
        .calendar_accounts()?
        .into_iter()
        .find(|account| account.id == id)
        .ok_or_else(|| "the account was kept but did not read back".to_owned())?;
    // What it holds now; a server that is slow to answer is asked again
    // by the first read.
    let _ = sync(root, reach, &account);
    Ok(view(root, &account))
}

/// The calendar account `service` names, its sign-in kept in the workspace's vault and no
/// record kept yet: what a mail account added for a provider that offers calendars keeps
/// beside itself, in the same change as its own record.
pub(super) fn prepare_service(
    reach: &Asking,
    service: Service,
) -> Result<accounts::Account, String> {
    prepared(
        reach,
        Add {
            server_url: service.server_url,
            username: service.username,
            password: service.password,
            name: Some(service.name),
            read_only: false,
        },
        None,
        Some(&service.mail_account),
    )
}

/// The record of the calendar account `adding` names, its sign-in kept in the workspace's
/// vault ([`keep_sign_in`]): what [`add`] keeps in the accounts, and what a mail account
/// keeps in the same change as its own record.
fn prepared(
    reach: &Asking,
    adding: Add,
    named: Option<ItemRef>,
    mail_account: Option<&str>,
) -> Result<accounts::Account, String> {
    let username = adding.username.trim().to_owned();
    if username.is_empty() || adding.password.is_empty() {
        return Err("A calendar account has a user name and a password".into());
    }
    let server = server_of(&adding.server_url)?;
    let id = account_id(&username, &server);
    // An account connected for reading only stays so when it is added
    // again, whoever adds it: only its person's removing it ends that.
    let read_only = adding.read_only
        || reach
            .calendar_accounts()?
            .iter()
            .any(|kept| kept.id == id && kept.read_only);
    let mut asked = accounts::Account::new(accounts::AccountId::from(id.clone()), TYPE_ID);
    asked.config = serde_json::json!({ "server_url": server.as_str(), "username": username });
    let found = FindingAccountTester(reach.reach.clone())
        .discover_calendars(&asked, &accounts::Secret::password(adding.password.clone()))?;
    let calendars: Vec<RemoteCalendar> = found
        .into_iter()
        .enumerate()
        .map(|(place, calendar)| RemoteCalendar {
            id: calendar.href,
            name: calendar.name,
            color: u8::try_from(place % 7).unwrap_or_default(),
            visible: true,
            // Every calendar of an account connected for reading only is
            // kept as one that cannot be written, whatever its server lets
            // the person do: whoever reads the account finds it so.
            read_only: calendar.read_only || read_only,
        })
        .collect();
    let display_name = adding
        .name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| username.clone());
    let mut stored = accounts::Account::new(accounts::AccountId::from(id), TYPE_ID);
    stored.display_name.clone_from(&display_name);
    stored.config = serde_json::json!({
        "display_name": display_name,
        "server_url": server.to_string(),
        "username": username,
        "calendars": calendars,
        "collapsed": false,
    });
    if read_only && let Some(config) = stored.config.as_object_mut() {
        config.insert("read_only".to_owned(), true.into());
    }
    // The mail account whose calendars these are: what reads the link back
    // when the account's services, or the account itself, go.
    if let Some(mail) = mail_account
        && let Some(config) = stored.config.as_object_mut()
    {
        config.insert("mail_account".to_owned(), mail.into());
    }
    let sign_in = match named {
        Some(named) => SignIn::Named(named),
        None => SignIn::Own(zeroize::Zeroizing::new(adding.password)),
    };
    keep_sign_in(reach, stored, sign_in)
}

/// The sign-in an account added here names.
enum SignIn {
    /// An entry a member keeps in the workspace's vault: it stays theirs.
    Named(ItemRef),
    /// A password typed for the account, kept as its own entry.
    Own(zeroize::Zeroizing<String>),
}

/// Keep `sign_in` for `stored` in the workspace's vault, naming it in the record it answers:
/// a member's entry, or a password kept as the account's own entry at its server's origin, in
/// place of the one it kept before. The record itself is the caller's to keep, so one change
/// holds every account an action makes.
fn keep_sign_in(
    reach: &Asking,
    mut stored: accounts::Account,
    sign_in: SignIn,
) -> Result<accounts::Account, String> {
    let vault = reach.vault.at();
    let owner = stored.id.to_string();
    let before = reach
        .accounts()?
        .account(stored.id.as_str())
        .and_then(|kept| sign_in_of(&kept.config));
    let reference = match sign_in {
        SignIn::Named(named) => named,
        SignIn::Own(password) => {
            let places = places_of(&stored)?;
            let username = stored
                .config
                .get("username")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            reach.runtime.block_on(vault.keep_sign_in(
                &owner,
                (accounts::vault_items::ItemKind::Login, places),
                &username,
                password,
                before.as_ref(),
            ))?
        }
    };
    accounts::email::set_password_item(&mut stored.config, Some(&reference.to_string()));
    if let Some(before) = before.filter(|before| *before != reference) {
        reach
            .runtime
            .block_on(vault.forget_sign_in(&owner, &before))?;
    }
    Ok(stored)
}

/// The place a calendar account's sign-in belongs to: its server's own
/// origin, and nothing else.
fn sign_in_place(adding: &AddWithItem) -> Result<accounts::vault_items::Place, String> {
    let server = server_of(&adding.server_url)?;
    accounts::vault_items::Place::parse(server.as_str()).map_err(|error| error.to_string())
}

/// Add an account with `sign_in`, what the vault item `adding` names
/// released for the server's own origin ([`sign_in_place`]): the account
/// names that item, and no copy of it is kept.
fn add_with_item(
    root: &Path,
    reach: &Asking,
    adding: AddWithItem,
    mut sign_in: accounts::vault_items::Released,
) -> Result<AccountView, String> {
    let username = adding
        .username
        .filter(|username| !username.trim().is_empty())
        .unwrap_or_else(|| std::mem::take(&mut sign_in.username));
    add(
        root,
        reach,
        Add {
            server_url: adding.server_url,
            username,
            password: sign_in.secret.to_string(),
            name: adding.name,
            read_only: adding.read_only,
        },
        Some(adding.password),
        None,
    )
}

impl Workspaces {
    /// Add `adding` to `workspace`'s calendar accounts, kept under `root`, with the sign-in its
    /// vault item keeps: a login that belongs to the server's own origin.
    async fn calendar_with_item(
        &self,
        (workspace, root): (&str, &Path),
        adding: AddWithItem,
        (by, asker): (&str, &tilo_action_policy::Acting),
    ) -> Result<AccountView, String> {
        let place = sign_in_place(&adding)?;
        let sign_in = self
            .release_item(
                workspace,
                &adding.password,
                (
                    accounts::vault_items::ItemKind::Login,
                    std::slice::from_ref(&place),
                ),
                (by, asker),
                accounts::vault_items::UsedFor::default(),
            )
            .await?;
        let root = root.to_path_buf();
        let reach = Asking::of(
            self.vault_for(workspace, asker),
            by,
            self.inner.models.mail_reach(),
        );
        off_thread(move || add_with_item(&root, &reach, adding, sign_in)).await
    }
}

async fn remove((vault, root): (VaultAt<'_>, &Path), account: &str) -> Result<Listed, String> {
    remove_account((vault, root), account).await?;
    Ok(Listed {
        accounts: accounts_in(&vault.accounts().await?)
            .iter()
            .map(|account| view(root, account))
            .collect(),
    })
}

/// Remove the calendar account `account`, its sign-in and what was kept of it.
pub(super) async fn remove_account(
    (vault, root): (VaultAt<'_>, &Path),
    account: &str,
) -> Result<(), String> {
    if !accounts_in(&vault.accounts().await?)
        .iter()
        .any(|kept| kept.id == account)
    {
        return Err("That calendar account is not kept here".into());
    }
    // The account and its own sign-in go together.
    Box::pin(super::remove_mail((vault, root), account)).await?;
    let _ = std::fs::remove_file(kept_at(root, account));
    Ok(())
}

/// A calendar account kept as one of a mail account's services: where its
/// provider keeps calendars, and the sign-in its person typed for the mail
/// account, which iCloud takes for both.
pub(super) struct Service {
    pub server_url: String,
    pub username: String,
    pub password: String,
    pub name: String,
    /// The mail account whose service this is.
    pub mail_account: String,
}

/// The calendar account `service` names, its sign-in kept, prepared where its server is
/// asked: what a mail account added for a provider that offers calendars keeps beside its
/// own record, in the same change, so one connect feeds Mail and Calendar alike.
pub(super) async fn prepare_service_in(
    (vault, _root): (VaultAt<'_>, &Path),
    reach: Reach,
    by: &str,
    service: Service,
) -> Result<accounts::Account, String> {
    let asking = Asking::of(vault, by, reach);
    off_thread(move || prepare_service(&asking, service)).await
}

/// Forget what was kept on this device of the calendar account `id`, when its
/// account goes: its copy of what its server held.
pub(super) fn forget_kept(root: &Path, id: &str) {
    let _ = std::fs::remove_file(kept_at(root, id));
}

/// The calendar account kept as the mail account `mail`'s service: the one it
/// names, else the one kept for it.
pub(super) fn linked_to(
    held: &accounts::person::AccountList,
    named: Option<&str>,
    mail: &str,
) -> Option<String> {
    let accounts = accounts_in(held);
    if let Some(named) = named
        && accounts.iter().any(|account| account.id == named)
    {
        return Some(named.to_owned());
    }
    accounts
        .into_iter()
        .find(|account| {
            account
                .stored
                .config
                .get("mail_account")
                .and_then(Value::as_str)
                == Some(mail)
        })
        .map(|account| account.id)
}

/// Bring the copies of `accounts` up to date when they are older than a
/// reader should be answered from. A server that does not answer leaves
/// its copy, with why.
fn freshen(root: &Path, reach: &Asking, accounts: &[Account], fresh_ms: i64) {
    for account in accounts {
        if now_ms().saturating_sub(read_kept(root, &account.id).synced_at_ms) > fresh_ms {
            let _ = sync(root, reach, account);
        }
    }
}

/// An entry's `notes` as a read answers them: whole where they are short,
/// and else their beginning with a mark that says more follows.
fn bounded_notes(notes: &str) -> String {
    let notes = notes.trim();
    let mut bounded: String = notes.chars().take(NOTES_CHARS).collect();
    if notes.chars().nth(NOTES_CHARS).is_some() {
        bounded.push('\u{2026}');
    }
    bounded
}

fn entries(
    root: &Path,
    reach: &Asking,
    accounts: &[Account],
    asked: &SpanAsked,
    fresh_ms: i64,
) -> Result<Value, String> {
    freshen(root, reach, accounts, fresh_ms);
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
        // What the entry says in its notes, from the text its server holds:
        // other people's words, as its title and its place are.
        if let Some(of) = held
            .iter()
            .find(|held| held.event.id.to_string() == occurrence.entry)
        {
            occurrence.notes = bounded_notes(&tilolang_calendar::ics::notes(&of.ics));
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

fn free(
    root: &Path,
    reach: &Asking,
    accounts: &[Account],
    asked: &FreeAsked,
) -> Result<Value, String> {
    freshen(root, reach, accounts, FRESH_MS);
    let held = held(root, accounts);
    let events: Vec<&Event> = held.iter().map(|held| &held.event).collect();
    serde_json::to_value(asked.answer(&events)?).map_err(|error| error.to_string())
}

/// Write `ics` to `url` on `account`'s server under `precondition`.
/// `Ok(false)` when the server holds something else there than expected.
fn write(
    root: &Path,
    reach: &Asking,
    account: &Account,
    url: &str,
    ics: String,
    precondition: Precondition<'_>,
) -> Result<bool, String> {
    // Nothing is written to the server of an account connected for reading
    // only, whoever asks and whatever was settled before this.
    if account.read_only {
        return Err(READ_ONLY.into());
    }
    let (client, server) = client(account, reach.sign_in(root, account)?, &reach.reach)?;
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
/// the person may write. Never one of an account connected for reading
/// only.
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
        // Nothing can be written, and an account connected for reading
        // only is why: say that.
        None if accounts.iter().any(|account| account.read_only) => READ_ONLY.to_owned(),
        None => "The person keeps no calendar that can be written".to_owned(),
    })?;
    if found.0.read_only {
        return Err(READ_ONLY.into());
    }
    if found.1.read_only {
        return Err("That calendar is read-only".into());
    }
    Ok(found)
}

/// The agent that asks about a workspace's calendars, where one does.
struct Assistant {
    workspace: String,
    agent: String,
    /// Whether the workspace's policy asks its person before it changes a
    /// calendar: what it writes then waits for their answer even where its
    /// judging lets it go.
    asks: bool,
}

impl Assistant {
    /// This agent, as an entry it puts in is kept under.
    fn putting(&self) -> PutIn {
        PutIn {
            workspace: self.workspace.clone(),
            agent: self.agent.clone(),
        }
    }
}

/// The entry a calendar holds under an invitation's identity.
struct Found {
    url: String,
    etag: Option<String>,
    /// Whom it names, as its server holds it.
    named: tilolang_calendar::ics::Named,
    /// The agent that put it in, where one did.
    put_in: Option<PutIn>,
}

impl Found {
    fn of(held: &Held) -> Self {
        Self {
            url: held.url.clone(),
            etag: held.etag.clone(),
            named: tilolang_calendar::ics::named(&held.ics),
            put_in: held.put_in.clone(),
        }
    }
}

/// What an invitation does to the calendar it goes into.
enum Change {
    /// Nothing: the calendar holds it as it is, or holds nothing it takes
    /// back.
    Nothing,
    /// A new entry, at this address.
    Create(String),
    /// The entry the calendar holds gets the invitation's text.
    Update(Found),
    /// The entry the calendar holds goes.
    Remove(Found),
}

/// What putting an invitation into a calendar comes to, read from the
/// calendar's server before anything is written there.
struct Planned {
    account: Account,
    /// The calendar it goes into, by its name.
    calendar: String,
    /// The entry's id, as a read answers it.
    entry: String,
    title: String,
    /// When it starts, as its text says.
    start: String,
    /// The text as the calendar would keep it.
    text: String,
    /// Whom that text names.
    named: tilolang_calendar::ics::Named,
    change: Change,
}

/// What putting the event `invitation` describes into a calendar comes to:
/// created under the invitation's own identity, so the same invitation
/// again finds it. Nothing is written yet.
fn planned(
    root: &Path,
    reach: &Asking,
    accounts: &[Account],
    invitation: &Invitation,
) -> Result<Planned, String> {
    let tilolang_calendar::ics::Invitation { event, cancels } =
        tilolang_calendar::ics::invitation(&invitation.ics)?;
    let (account, calendar) = writable(accounts, invitation.calendar.as_deref())?;
    let _ = sync(root, reach, account);
    let existing = held(root, std::slice::from_ref(account))
        .into_iter()
        .find(|held| held.event.id == event.id);
    // What a line says of who writes to the person it names is settled
    // against what the server holds of them (RFC 6638, 3.2.1.2), as just
    // read, and is written over that version only ([`carry_out`]).
    let text = tilolang_calendar::ics::kept_beside(
        &invitation.ics,
        existing.as_ref().map(|held| held.ics.as_str()),
    );
    let (change, text) = match (existing, cancels) {
        (None, true) => (Change::Nothing, text),
        (Some(held), true) => (Change::Remove(Found::of(&held)), text),
        (Some(held), false) => {
            let same = tilolang_calendar::ics::parse(&text)
                .0
                .into_iter()
                .next()
                .is_some_and(|mut sent| {
                    sent.calendar_id.clone_from(&held.event.calendar_id);
                    sent == held.event
                });
            if same {
                (Change::Nothing, text)
            } else {
                // What its person filed it under on the server stays.
                let kept: Vec<String> = held.event.categories.iter().cloned().collect();
                let text = if event.categories.is_empty() && !kept.is_empty() {
                    tilolang_calendar::ics::with_categories(&text, &kept).unwrap_or(text)
                } else {
                    text
                };
                (Change::Update(Found::of(&held)), text)
            }
        }
        (None, false) => {
            let url = format!("{}/{}.ics", calendar.id.trim_end_matches('/'), event.id);
            (Change::Create(url), text)
        }
    };
    Ok(Planned {
        account: account.clone(),
        calendar: calendar.name.clone(),
        entry: event.id.to_string(),
        start: format!(
            "{} ({})",
            event.start.strftime("%Y-%m-%d %H:%M"),
            event.timezone
        ),
        title: event.title,
        named: tilolang_calendar::ics::named(&text),
        text,
        change,
    })
}

/// Write what `planned` comes to on its calendar's server, and answer what
/// was made of the invitation. An entry that is no longer what was read
/// when it was planned is not written over.
fn carry_out(root: &Path, reach: &Asking, planned: Planned) -> Result<Value, String> {
    let Planned {
        account,
        entry,
        text,
        named,
        change,
        ..
    } = planned;
    let made = match change {
        Change::Nothing => "unchanged",
        Change::Remove(found) => {
            if account.read_only {
                return Err(READ_ONLY.into());
            }
            let (client, server) = client(&account, reach.sign_in(root, &account)?, &reach.reach)?;
            let matching = found.etag.as_deref().unwrap_or("*");
            // The server answers nobody for its person: where they were
            // invited, declining is theirs to say.
            let response = client
                .send(
                    "DELETE",
                    on_server(&server, &found.url)?,
                    &[("Schedule-Reply", "F")],
                    tilo_dav::client::Body::Empty,
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
        Change::Update(found) => {
            // What the text says of who writes to the people it names was
            // settled against one version of the entry (RFC 6638, 3.2.1.2),
            // so it goes over that version and no other.
            let names = !(named.organizers.is_empty() && named.attendees.is_empty());
            let matching = match found.etag.as_deref() {
                Some(version) => version,
                None if names => {
                    return Err(
                        "The calendar server gave no version of this entry, so what it holds \
                         of the people the entry names could not be kept: nothing was written"
                            .into(),
                    );
                }
                None => "*",
            };
            if !write(
                root,
                reach,
                &account,
                &found.url,
                text,
                Precondition::IfMatch(matching),
            )? {
                return Err("The entry changed on its server meanwhile: ask again".into());
            }
            "updated"
        }
        Change::Create(url) => {
            if !write(
                root,
                reach,
                &account,
                &url,
                text,
                Precondition::IfNoneMatchAny,
            )? {
                return Err("The entry changed on its server meanwhile: ask again".into());
            }
            "created"
        }
    };
    if made != "unchanged" {
        let _ = sync(root, reach, &account);
    }
    Ok(serde_json::json!({ "entry": entry, "made": made }))
}

/// Put the event `invitation` describes into a calendar, for the person
/// whose calendar it is: their asking is their word.
fn add_invitation(
    root: &Path,
    reach: &Asking,
    accounts: &[Account],
    invitation: &Invitation,
) -> Result<Value, String> {
    carry_out(root, reach, planned(root, reach, accounts, invitation)?)
}

/// Keep that the agent `by` put in the entry `entry` of `account`, as the
/// pull after its writing found it: what lets that agent bring it up to date
/// and take it out again. Where that pull found no such entry nothing is
/// kept, and the entry is its person's own.
fn note_put_in(root: &Path, account: &Account, entry: &str, by: PutIn) {
    let Some(found) = held(root, std::slice::from_ref(account))
        .into_iter()
        .find(|held| held.event.id.to_string() == entry)
    else {
        return;
    };
    let mut kept = read_kept(root, &account.id);
    let Some(calendar) = kept.calendars.get_mut(&found.calendar) else {
        return;
    };
    calendar.put_in.insert(found.url, by);
    if let Err(error) = write_kept(root, &account.id, &kept) {
        tracing::warn!(%error, "calendars: whose putting an entry is could not be kept");
    }
}

/// The addresses the person who keeps `account` is written to at, as far as
/// this holder knows them, in lower case: the name the account signs in
/// with, where that is an address, and the mail accounts they keep here.
fn own_addresses(
    held: &accounts::person::AccountList,
    root: &Path,
    account: &Account,
) -> BTreeSet<String> {
    let mut own: BTreeSet<String> = mail_accounts(held, root)
        .accounts
        .into_iter()
        .map(|mail| mail.address.trim().to_ascii_lowercase())
        .collect();
    own.insert(account.username.trim().to_ascii_lowercase());
    own.retain(|address| {
        address
            .split_once('@')
            .is_some_and(|(user, domain)| !user.is_empty() && !domain.is_empty())
    });
    own
}

/// Whether an entry that names `named` is an invitation to the person
/// written to at `own`: one organiser, who is somebody else, and an address
/// of the person's among those it invites. Where this holder knows no
/// address of the person's, nothing is.
fn invites(named: &tilolang_calendar::ics::Named, own: &BTreeSet<String>) -> bool {
    match named.organizers.as_slice() {
        [organizer] => {
            !own.contains(organizer) && named.attendees.iter().any(|invited| own.contains(invited))
        }
        _ => false,
    }
}

/// What an invitation an agent hands over comes to before anything is
/// written.
enum Judged {
    /// It goes ahead.
    Goes,
    /// Its person is asked first, and shown these rows.
    Asks(Vec<(String, String)>),
}

impl Judged {
    /// This, asked of its person all the same where `asks`: the workspace's
    /// policy has them asked before an agent changes a calendar.
    fn or_asked(self, asks: bool) -> Self {
        match self {
            Self::Goes if asks => Self::Asks(Vec::new()),
            judged => judged,
        }
    }
}

/// What an agent may do with `planned` in its workspace's calendar,
/// whose text is the agent's own writing, for the person written to at
/// `own`.
///
/// An entry the calendar holds is changed or removed only where it is an
/// invitation to that person ([`invites`]), the text names the organiser it
/// has, and the agent `by` put it in itself. The organiser a text names is
/// the agent's own writing and proves nothing, so an invitation that reached
/// the calendar another way (its server filed it, or another client did) is
/// not the agent's. Any other entry is its person's own, and that is the
/// refusal. A new entry goes ahead where it names nobody or is an invitation
/// to the person; one that names people otherwise is put to the person first
/// ([`shown`]), or refused where a question could not show all of it.
fn judged(planned: &Planned, own: &BTreeSet<String>, by: &PutIn) -> Result<Judged, String> {
    let named = &planned.named;
    let names_nobody = named.organizers.is_empty() && named.attendees.is_empty();
    match &planned.change {
        Change::Nothing => Ok(Judged::Goes),
        Change::Update(found) | Change::Remove(found) => {
            if !invites(&found.named, own) || found.named.organizers != named.organizers {
                Err(THEIRS.to_owned())
            } else if found.put_in.as_ref() == Some(by) {
                Ok(Judged::Goes)
            } else {
                Err(ANOTHER_WAY.to_owned())
            }
        }
        Change::Create(_) if names_nobody || invites(named, own) => Ok(Judged::Goes),
        Change::Create(_) => shown(planned).map(Judged::Asks),
    }
}

/// What a person is shown of an entry that names people before it is put
/// into their calendar: where it goes, what and when it is, how many
/// addresses it names and each of them in a row of its own, as the text
/// that is then written names them. Why it cannot be shown so, where it
/// cannot: nobody is asked about such an entry.
fn shown(planned: &Planned) -> Result<Vec<(String, String)>, String> {
    let named = &planned.named;
    let addresses = named.organizers.len().saturating_add(named.attendees.len());
    if addresses > sending::MOST_ADDRESSES {
        return Err(format!(
            "This entry names {addresses} addresses, and its person is shown at most {} before \
             an entry that names people is put into their calendar. Put it in with fewer, or \
             leave it to them to enter.",
            sending::MOST_ADDRESSES
        ));
    }
    let mut rows = vec![("Calendar".to_owned(), planned.calendar.clone())];
    sending::by_its_start(&mut rows, "Title", planned.title.trim(), TITLE_CHARS);
    rows.push(("Starts".to_owned(), planned.start.clone()));
    rows.push((
        "Names".to_owned(),
        sending::counted(addresses, "address", "addresses"),
    ));
    let lists = [
        ("Organised by", &named.organizers),
        ("Invites", &named.attendees),
    ];
    for (label, list) in lists {
        for address in list {
            if !sending::reads_plainly(address) {
                let start: String = address.chars().take(sending::ADDRESS_CHARS).collect();
                return Err(format!(
                    "The address {start:?} cannot be shown to its person so that they read it \
                     for what it is: it holds letters outside plain ASCII or a quoted part, or \
                     is longer than {} characters. An entry that names it is theirs to enter.",
                    sending::ADDRESS_CHARS
                ));
            }
            rows.push((label.to_owned(), address.clone()));
        }
    }
    let bytes: usize = rows
        .iter()
        .map(|(label, value)| label.len().saturating_add(value.len()))
        .sum();
    if bytes > sending::QUESTION_BYTES {
        return Err(
            "This entry names more than one question can show its person before it is put \
             into their calendar."
                .into(),
        );
    }
    Ok(rows)
}

/// What the person whose account `account` is is shown about what their agent, asking as
/// `caller`, would do (`verb`, on `plural`): `rows`, every address the entry names.
fn facts(
    caller: &Caller,
    account: &Account,
    (verb, plural): (&str, &str),
    rows: Vec<(String, String)>,
) -> AskedFacts {
    AskedFacts {
        on: Some(account.name.clone()),
        facts: rows
            .into_iter()
            .map(|(label, value)| Fact::new(label, value))
            .collect(),
        verb: verb.to_owned(),
        plural: Some(plural.to_owned()),
        ..AskedFacts::by(crate::workspaces::asking::asked_by(caller))
    }
}

/// Where a grant that lets an agent do `action` to its person's calendars unasked stands in
/// `workspace`'s policy. For [`ADD_INVITATION`] that is putting in entries that name people,
/// for [`SET_CATEGORIES`] filing what is its person's own.
fn node_of(workspace: &str, action: &str) -> tilo_action_policy::TreePath {
    tilo_action_policy::TreePath::workspace(workspace)
        .app(person_accounts::PROVIDER)
        .kind(person_accounts::PROVIDER)
        .resource(RESOURCE)
        .action(action)
}

/// An entry about to be filed under categories, as its server held it when
/// that was read: nothing is written yet.
struct Filing {
    account: Account,
    /// The calendar it is in, by its name.
    calendar: String,
    /// The entry's id, as a read answers it.
    entry: String,
    title: String,
    /// When it starts, as its text says.
    start: String,
    /// What it is filed under now.
    filed: Vec<String>,
    /// What it is to be filed under.
    categories: Vec<String>,
    url: String,
    etag: Option<String>,
    /// The text as the calendar would keep it: its server's own, with the
    /// one field changed.
    text: String,
    /// Whether its lines leave writing to somebody to its server.
    leaves_writing: bool,
    /// The agent that put it in, where one did.
    put_in: Option<PutIn>,
}

/// What filing the entry `asked` names under its categories comes to, in
/// the text its server holds: every other line of the entry stays as the
/// server has it. Nothing is written yet.
fn filing(
    root: &Path,
    reach: &Asking,
    accounts: &[Account],
    asked: &Categories,
) -> Result<Filing, String> {
    if asked.categories.len() > tilolang_calendar::model::MAX_CATEGORIES {
        return Err("an entry has at most 32 categories".into());
    }
    let categories = asked
        .categories
        .iter()
        .map(|named| tilolang_calendar::model::category(named))
        .collect::<Option<Vec<String>>>()
        .ok_or("a category is a few words of at most 64 characters")?;
    freshen(root, reach, accounts, FRESH_MS);
    let held = held(root, accounts)
        .into_iter()
        .find(|held| held.event.id.to_string() == asked.entry)
        .ok_or("That entry is in none of the person's calendars")?;
    let account = accounts
        .iter()
        .find(|account| account.id == held.account)
        .ok_or("That entry is in none of the person's calendars")?;
    // Filing rewrites the entry on its server: not for an account connected
    // for reading only, and nobody is asked about it.
    if account.read_only {
        return Err(READ_ONLY.into());
    }
    let text = tilolang_calendar::ics::with_categories(&held.ics, &categories)
        .ok_or("the entry as its server holds it does not read")?;
    let calendar = account
        .calendars
        .iter()
        .find(|one| one.id == held.calendar)
        .map_or_else(String::new, |one| one.name.clone());
    Ok(Filing {
        account: account.clone(),
        calendar,
        entry: asked.entry.clone(),
        start: format!(
            "{} ({})",
            held.event.start.strftime("%Y-%m-%d %H:%M"),
            held.event.timezone
        ),
        filed: held.event.categories.iter().cloned().collect(),
        leaves_writing: tilolang_calendar::ics::leaves_writing_to_its_server(&held.ics),
        title: held.event.title,
        categories,
        url: held.url,
        etag: held.etag,
        text,
        put_in: held.put_in,
    })
}

/// What the agent `by`, which a person named for their calendar, may do
/// with `filing`. Filing rewrites the entry on its server, so it is held to
/// what every other change of an entry is. An entry whose lines leave
/// writing to somebody to its server is not the agent's to rewrite at all:
/// that server may write to them, and no question shows its person whom.
/// What the agent put in itself it files. Every other entry is its person's
/// own, and is filed once the person was shown the entry and the words
/// ([`Judged::Asks`]).
fn filed(filing: &Filing, by: &PutIn) -> Result<Judged, String> {
    if filing.leaves_writing {
        return Err(SERVER_WRITES.to_owned());
    }
    if filing.put_in.as_ref() == Some(by) {
        return Ok(Judged::Goes);
    }
    let mut rows = vec![("Calendar".to_owned(), filing.calendar.clone())];
    sending::by_its_start(&mut rows, "Title", filing.title.trim(), TITLE_CHARS);
    rows.push(("Starts".to_owned(), filing.start.clone()));
    for held in &filing.filed {
        rows.push(("Filed under".to_owned(), held.clone()));
    }
    if filing.categories.is_empty() {
        rows.push(("To file under".to_owned(), "Nothing".to_owned()));
    }
    for named in &filing.categories {
        rows.push(("To file under".to_owned(), named.clone()));
    }
    let bytes: usize = rows
        .iter()
        .map(|(label, value)| label.len().saturating_add(value.len()))
        .sum();
    if bytes > sending::QUESTION_BYTES {
        return Err("This entry is filed under more than one question can show its person.".into());
    }
    Ok(Judged::Asks(rows))
}

/// Write what `filing` comes to on its calendar's server. An entry that is
/// no longer what was read when it was planned is not written over.
fn file(root: &Path, reach: &Asking, filing: Filing) -> Result<Value, String> {
    let Filing {
        account,
        entry,
        categories,
        url,
        etag,
        text,
        ..
    } = filing;
    let matching = etag.as_deref().unwrap_or("*");
    if !write(
        root,
        reach,
        &account,
        &url,
        text,
        Precondition::IfMatch(matching),
    )? {
        return Err("The entry changed on its server meanwhile: ask again".into());
    }
    let _ = sync(root, reach, &account);
    Ok(serde_json::json!({ "entry": entry, "categories": categories }))
}

/// File the entry `asked` names under its categories, for the person whose
/// calendar it is: their asking is their word.
fn set_categories(
    root: &Path,
    reach: &Asking,
    accounts: &[Account],
    asked: &Categories,
) -> Result<Value, String> {
    file(root, reach, filing(root, reach, accounts, asked)?)
}

/// What `action` does to a workspace's calendars, as its policy weighs it.
fn calendar_effect(action: &str) -> tilo_action_policy::ActionKind {
    if [LIST, SYNC, ENTRIES, FREE].contains(&action) {
        tilo_action_policy::ActionKind::Read
    } else {
        tilo_action_policy::ActionKind::Undoable
    }
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
    /// Every occurrence between `from_ms` and `to_ms` in the calendar
    /// accounts `workspace` keeps, each as [`ENTRIES`] answers one: what an
    /// automation that waits on an entry's time reads before it tells an
    /// agent of `person`'s. Nothing where the workspace's policy does not let
    /// `person` read its calendars, so an automation hands an agent nothing
    /// its person may not read. A copy older than five minutes is brought up
    /// to date first, and a server that does not answer leaves the copy as it
    /// stood.
    ///
    /// # Errors
    /// The span does not read, or the read did not end.
    pub async fn calendar_occurrences_for(
        &self,
        workspace: collab_types::WorkspaceId,
        person: &[u8; 32],
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<Value>, String> {
        if !self
            .person_reaches(
                workspace,
                person,
                Store::CALENDARS,
                ENTRIES,
                tilo_action_policy::ActionKind::Read,
            )
            .await
        {
            return Ok(Vec::new());
        }
        let root = self.workspace_root(&workspace.to_string());
        let accounts = accounts_in(&self.workspace_accounts(&workspace.to_string()).await?);
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let asked = SpanAsked::between_ms(from_ms, to_ms)
            .ok_or_else(|| "a time no calendar reaches".to_owned())?;
        let workspace = workspace.to_string();
        let acting = tilo_action_policy::Acting::person(hex::encode(person));
        let reach = Asking::of(
            self.vault_for(&workspace, &acting),
            "",
            self.inner.models.mail_reach(),
        );
        off_thread(move || {
            let mut read = entries(&root, &reach, &accounts, &asked, TIMES_FRESH_MS)?;
            match read.get_mut("entries").map(Value::take) {
                Some(Value::Array(occurrences)) => Ok(occurrences),
                _ => Ok(Vec::new()),
            }
        })
        .await
    }

    /// Add a calendar account with the sign-in a vault item keeps. A
    /// sign-in named by reference carries none, so whoever the workspace's
    /// policy lets may ask: the vault decides who uses it and where.
    async fn calendar_by_reference(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionError> {
        let reach = self
            .reaching_allowed(
                (workspace_id, caller, certificate),
                Store::CALENDARS,
                ADD_WITH_ITEM,
                tilo_action_policy::ActionKind::Undoable,
                request,
            )
            .await?;
        let by = reach.by();
        let root = self.workspace_root(&reach.workspace.to_string());
        let adding: AddWithItem = argument(
            request,
            "a calendar account names its server and the vault item that is its password",
        )?;
        let kept = self
            .calendar_with_item(
                (&reach.workspace.to_string(), &root),
                adding,
                (&by, &reach.acting),
            )
            .await;
        if kept.is_ok() {
            self.person_settings_changed();
        }
        answer(&kept)
    }

    /// The calendars of the workspace `caller` asks about with `action`, as
    /// its policy lets the caller: every account it keeps, and the agent
    /// asking where one does, answered as that agent ([`Self::assistant_adds`]).
    /// An agent's entry of its own writing is judged before its person is
    /// asked about it, so the policy's question about that is left to the
    /// judging ([`Assistant::asks`]); every other question is put first.
    async fn calendars_asked(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<(PathBuf, Vec<Account>, Option<Assistant>, Asking), ActionError> {
        let action = request.target.address.action.as_str();
        let effect = calendar_effect(action);
        let reach = self
            .reaching(
                (workspace_id, caller, certificate),
                Store::CALENDARS,
                action,
                effect,
            )
            .await?;
        let judged_first =
            reach.agent.is_some() && [ADD_INVITATION, SET_CATEGORIES].contains(&action);
        let reach = if judged_first {
            reach
        } else {
            self.allowed(reach, caller, Store::CALENDARS, action, request)
                .await?
        };
        let workspace = reach.workspace.to_string();
        let root = self.workspace_root(&workspace);
        let accounts = accounts_in(
            &self
                .workspace_accounts(&workspace)
                .await
                .map_err(|reason| ActionError::Refused { reason })?,
        );
        let asking = Asking::of(
            self.vault_for(&workspace, &reach.acting),
            &reach.by(),
            self.inner.models.mail_reach(),
        );
        let assistant = reach.agent.clone().map(|agent| Assistant {
            workspace,
            agent,
            asks: reach.answer != tilo_action_policy::Answer::Allow,
        });
        Ok((root, accounts, assistant, asking))
    }

    /// Put an invitation in for the agent its person named for the account,
    /// whose text is the agent's own writing ([`judged`]): what is its
    /// person's own is refused, and what names people and is no invitation
    /// to its person waits for the person's answer, unless they let it stay
    /// allowed. Only then is anything written, and what is written is what
    /// was read before they were asked.
    async fn assistant_adds(
        &self,
        (root, reach): (PathBuf, Asking),
        accounts: Vec<Account>,
        invitation: Invitation,
        assistant: &Assistant,
        (caller, request): (&Caller, &InvokeRequest),
    ) -> Result<Value, ActionError> {
        let reading = root.clone();
        let by = assistant.putting();
        let asking = by.clone();
        let reaching = reach.clone();
        let read = off_thread(move || {
            let planned = planned(&reading, &reaching, &accounts, &invitation)?;
            let own = own_addresses(&reaching.accounts()?, &reading, &planned.account);
            let judged = judged(&planned, &own, &asking)?;
            Ok((planned, judged))
        })
        .await;
        let (planned, judged) = match read {
            Ok(read) => read,
            Err(reason) => return Err(ActionError::Refused { reason }),
        };
        if let Judged::Asks(rows) = judged.or_asked(assistant.asks)
            && !self
                .granted(
                    &assistant.workspace,
                    &assistant.agent,
                    &node_of(&assistant.workspace, ADD_INVITATION),
                )
                .await
        {
            crate::workspaces::asking::approved_or_asks(request, || {
                facts(
                    caller,
                    &planned.account,
                    ("put in", "calendar entries"),
                    rows,
                )
            })?;
        }
        let creates = matches!(planned.change, Change::Create(_));
        let made = off_thread(move || {
            let (account, entry) = (planned.account.clone(), planned.entry.clone());
            let made = carry_out(&root, &reach, planned)?;
            // What it put in is its own to bring up to date and take out.
            if creates {
                note_put_in(&root, &account, &entry, by);
            }
            Ok(made)
        })
        .await;
        answer(&made)
    }

    /// File an entry under categories for the agent its person named for the
    /// account ([`filed`]): what it put in itself is filed, what would have
    /// a calendar server write to people is refused, and what is its
    /// person's own waits for the person's answer, unless they let it stay
    /// allowed. Only then is anything written, and what is written is what
    /// was read before they were asked.
    async fn assistant_files(
        &self,
        (root, reach): (PathBuf, Asking),
        accounts: Vec<Account>,
        asked: Categories,
        assistant: &Assistant,
        (caller, request): (&Caller, &InvokeRequest),
    ) -> Result<Value, ActionError> {
        let reading = root.clone();
        let by = assistant.putting();
        let reaching = reach.clone();
        let read = off_thread(move || {
            let filing = filing(&reading, &reaching, &accounts, &asked)?;
            let judged = filed(&filing, &by)?;
            Ok((filing, judged))
        })
        .await;
        let (filing, judged) = match read {
            Ok(read) => read,
            Err(reason) => return Err(ActionError::Refused { reason }),
        };
        if let Judged::Asks(rows) = judged.or_asked(assistant.asks)
            && !self
                .granted(
                    &assistant.workspace,
                    &assistant.agent,
                    &node_of(&assistant.workspace, SET_CATEGORIES),
                )
                .await
        {
            crate::workspaces::asking::approved_or_asks(request, || {
                facts(caller, &filing.account, ("file", "calendar entries"), rows)
            })?;
        }
        let made = off_thread(move || file(&root, &reach, filing)).await;
        answer(&made)
    }

    /// Keep what was asked, tell the person settings that changed where it
    /// was kept, and answer what it kept.
    async fn kept_and_answered<T: serde::Serialize>(
        &self,
        keeping: impl Future<Output = Result<T, String>>,
    ) -> Result<Value, ActionError> {
        let kept = keeping.await;
        if kept.is_ok() {
            self.person_settings_changed();
        }
        answer(&kept)
    }

    /// Answer a request about a workspace's calendars ([`RESOURCE`]), as its
    /// policy lets the caller.
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
        let (root, accounts, assistant, reach) = self
            .calendars_asked(workspace_id, caller, certificate, request)
            .await?;
        let listed = |root: &Path, accounts: &[Account]| Listed {
            accounts: accounts.iter().map(|account| view(root, account)).collect(),
        };
        match action.as_str() {
            LIST => answer(&Ok::<_, String>(listed(&root, &accounts))),
            ADD => {
                let adding: Add = argument(request, "a calendar account to add")?;
                self.kept_and_answered(off_thread(move || add(&root, &reach, adding, None, None)))
                    .await
            }
            REMOVE => {
                let asked: OneAccount = argument(request, "the account")?;
                self.kept_and_answered(remove(
                    (reach.vault.at(), &root),
                    asked.account.as_deref().unwrap_or_default(),
                ))
                .await
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
                        sync(&root, &reach, account)?;
                    }
                    Ok(listed(&root, &accounts))
                });
                answer(&pulled.await)
            }
            ENTRIES => {
                let asked: SpanAsked = argument(request, "a span to read")?;
                answer(
                    &off_thread(move || entries(&root, &reach, &accounts, &asked, FRESH_MS)).await,
                )
            }
            FREE => {
                let asked: FreeAsked = argument(request, "a span to find free time in")?;
                answer(&off_thread(move || free(&root, &reach, &accounts, &asked)).await)
            }
            ADD_INVITATION => {
                let asked: Invitation = argument(request, "an invitation to add")?;
                match assistant {
                    // Their own asking is their word.
                    None => answer(
                        &off_thread(move || add_invitation(&root, &reach, &accounts, &asked)).await,
                    ),
                    Some(assistant) => {
                        Box::pin(self.assistant_adds(
                            (root, reach),
                            accounts,
                            asked,
                            &assistant,
                            (caller, request),
                        ))
                        .await
                    }
                }
            }
            SET_CATEGORIES => {
                let asked: Categories = argument(request, "an entry and its categories")?;
                match assistant {
                    // Their own asking is their word.
                    None => answer(
                        &off_thread(move || set_categories(&root, &reach, &accounts, &asked)).await,
                    ),
                    Some(assistant) => {
                        Box::pin(self.assistant_files(
                            (root, reach),
                            accounts,
                            asked,
                            &assistant,
                            (caller, request),
                        ))
                        .await
                    }
                }
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}

/// What a test of something that reads a workspace's calendars keeps of them
/// without a calendar server at hand.
#[cfg(any(test, feature = "test-support"))]
impl Workspaces {
    /// Keep a calendar account in `workspace` as if its server had been
    /// pulled this moment: one calendar that holds `entries`, each the
    /// iCalendar text of one entry. Answers the account's id. Calling it again replaces what the calendar holds,
    /// as the next pull does after another client changed it.
    ///
    /// # Errors
    /// The account or the copy could not be kept.
    pub async fn keep_calendar_as_pulled(
        &self,
        workspace: &str,
        entries: &[String],
    ) -> Result<String, String> {
        const SERVER: &str = "https://calendar.example.test/dav/";
        const CALENDAR: &str = "https://calendar.example.test/dav/calendars/ada/work/";
        let root = self.workspace_root(workspace);
        let id = account_id("ada", &server_of(SERVER)?);
        // Its server is never asked: the copy is fresh, so the account names no sign-in.
        let mut stored = accounts::Account::new(accounts::AccountId::from(id.clone()), TYPE_ID);
        stored.display_name = "Work".into();
        stored.config = serde_json::json!({
            "display_name": "Work",
            "server_url": SERVER,
            "username": "ada",
            "calendars": [RemoteCalendar {
                id: CALENDAR.into(),
                name: "Work".into(),
                color: 0,
                visible: true,
                read_only: false,
            }],
            "collapsed": false,
        });
        self.keep_workspace_account((workspace, &self.acting_or_own(None)), &stored)
            .await?;
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
