//! Thunderbird on this computer as a migration source.
//!
//! The profile is read where it is, by the same reader the Tilo OS installer's migration uses
//! ([`migration_sources::thunderbird`]), and never written: the stores it keeps in databases
//! (its saved sign-ins' key, address books, calendars) are read from a private copy.
//!
//! Each account Thunderbird reads from a server is **connected**: its incoming and outgoing
//! servers, the names and addresses it sends as, and the password Thunderbird saved for it go to
//! the server that keeps the person's workspace, as their own request
//! (`person/mail-accounts#add`). That server signs in with the password first, and only once
//! the mail server took it keeps the account and seals the password in the person's vault. Mail
//! then syncs the account from its server, as it does any account. Nothing is copied from
//! Thunderbird's cache for it. Connecting is the person's to say: the migration shows each account
//! and its servers at their terminal and connects it once they say yes (or said `--yes`
//! beforehand). The password is never printed, logged or kept anywhere on this computer.
//!
//! Copying the messages Thunderbird keeps on this computer is a separate choice, made per
//! account (`cache/<address>`, or some of its folders): for an account whose server is gone,
//! or an archive kept offline. The review checks whether each account's server answers and
//! suggests the copy only for one that does not. Each copied message lands in the workspace's
//! Mail as it was sent, its MIME bytes whole (its large parts carried apart from the rest, so no
//! message is too large to come over), in its folder with its read state and its
//! conversation (Mail's `deliver-message`, which keeps it under the id its `Message-ID` names
//! and knows a message it already holds). Contacts and events land in Contacts and Calendar
//! under an id the source decides, so running it again names the same messages, contacts and events and adds nothing.

use std::collections::BTreeMap;
use std::net::ToSocketAddrs as _;
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::state::{Item, Review};
use super::{Landed, Lander};

/// The source name `tilo migrate` uses.
pub const SOURCE: &str = "thunderbird";

/// Where a message goes: the messages kept beside the workspace's mailbox, in its folder, which
/// Mail delivers to (the mailbox itself keeps folders, drafts and accounts).
const DELIVER: &str = "mail/mail.messages#deliver-message";
const ADD_CONTACTS: &str = "contacts/contacts.contacts#add-contacts";
const NEW_EVENT: &str = "calendar/calendar.calendar#new-event";
/// The person's mail accounts on the server that keeps their workspace, which only they add to.
pub const MAIL_ACCOUNTS: &str = "person/mail-accounts";
/// What the review names an account's cached messages by: `cache/<address>`.
const CACHE: &str = "cache/";
/// The kind of the item that copies an account's cache.
pub const CACHE_KIND: &str = "cache";

/// How many contacts one action carries.
const CONTACTS_PER_ACTION: usize = 100;
/// How long the review waits for an account's server to answer.
const ANSWERS_WITHIN: Duration = Duration::from_secs(4);

/// Whether a mail server answers at a host and port.
pub type Answers<'a> = &'a dyn Fn(&str, u16) -> bool;

/// Whether a server listens at `host`:`port` now: a connection to one of its addresses is
/// taken within [`ANSWERS_WITHIN`].
#[must_use]
pub fn server_answers(host: &str, port: u16) -> bool {
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return false;
    };
    addresses
        .take(4)
        .any(|address| std::net::TcpStream::connect_timeout(&address, ANSWERS_WITHIN).is_ok())
}

/// The review of the Thunderbird profile at `home`: `profile` names one when there are
/// several; the default profile otherwise. Nothing is chosen until the person says.
///
/// # Errors
/// No Thunderbird profile is here, or none that is here could be read; the
/// message names what could not be read and why.
pub fn review(home: &Path, profile: Option<&str>) -> Result<Review> {
    review_with(home, profile, &server_answers)
}

/// [`review`], asking `answers` whether an account's server is there.
///
/// # Errors
/// As [`review`].
pub fn review_with(home: &Path, profile: Option<&str>, answers: Answers<'_>) -> Result<Review> {
    let scan = migration_sources::thunderbird::scan(home);
    let reviewed = readable_profile(&scan, profile)?;
    let found = reviewed.profile();
    let mut items = Vec::new();
    for account in &found.accounts {
        account_items(account, answers, &mut items);
    }
    for book in &found.address_books {
        items.push(Item {
            key: format!("contacts/{}", book.name),
            kind: "contacts".into(),
            title: book.name.clone(),
            count: count(book.contacts),
            counts: "contacts".into(),
            within: None,
            chosen: false,
            note: None,
        });
    }
    for calendar in &found.calendars {
        items.push(Item {
            key: format!("calendar/{}", calendar.name),
            kind: "calendar".into(),
            title: calendar.name.clone(),
            count: count(calendar.events),
            counts: "events".into(),
            within: None,
            chosen: false,
            note: None,
        });
    }
    Ok(Review {
        source: SOURCE.into(),
        title: format!("Thunderbird profile {}", found.label),
        handle: found.source_id,
        unreadable: scan
            .problems
            .iter()
            .chain(&reviewed.problems)
            .map(ToString::to_string)
            .collect(),
        items,
    })
}

/// What `account` offers: connecting it when Mail can sync it from its server
/// (asking `answers` whether that server is there), and copying what
/// Thunderbird keeps of it, folder by folder.
fn account_items(
    account: &migration_sources::thunderbird::ThunderbirdAccount,
    answers: Answers<'_>,
    items: &mut Vec<Item>,
) {
    let key = account_key(account);
    let messages = account
        .folders
        .iter()
        .map(|folder| count(folder.messages))
        .sum();
    let servers = servers(account);
    let answering = servers
        .as_ref()
        .map(|servers| answers(&servers.imap_host, servers.imap_port));
    let cache = format!("{CACHE}{key}");
    let provider = signs_in_with(account);
    let title = match (provider, &servers) {
        (Some(provider), _) => Some(format!(
            "connect {} with {provider}'s sign-in, in Settings > External accounts; the \
             sign-in Thunderbird keeps for it is Thunderbird's own and is not reused",
            account.display_name
        )),
        (None, Some(servers)) => Some(format!(
            "connect {} ({}:{}, sends by {}:{}); Mail syncs it from its server",
            account.display_name,
            servers.imap_host,
            servers.imap_port,
            servers.smtp_host,
            servers.smtp_port
        )),
        (None, None) => None,
    };
    if let Some(title) = title {
        items.push(Item {
            key,
            kind: "account".into(),
            title,
            count: 0,
            counts: String::new(),
            within: None,
            chosen: false,
            note: servers
                .as_ref()
                .filter(|_| answering == Some(false))
                .map(|servers| {
                    format!(
                        "{}:{} did not answer: if the account is gone, copy what Thunderbird \
                         keeps instead (tilo migrate choose thunderbird {cache})",
                        servers.imap_host, servers.imap_port
                    )
                }),
        });
    }
    let note = match answering {
        None if provider.is_some() => None,
        None => Some(format!(
            "{} is no account Mail syncs from a server ({}): copying is the way to keep \
             its mail",
            account.display_name, account.kind
        )),
        Some(false) => Some("its server did not answer: copying keeps its mail".to_owned()),
        Some(true) => None,
    };
    items.push(Item {
        key: cache.clone(),
        kind: CACHE_KIND.into(),
        title: format!(
            "copy the messages of {} Thunderbird keeps on this computer",
            account.display_name
        ),
        count: messages,
        counts: "messages".into(),
        within: None,
        chosen: false,
        note,
    });
    for folder in &account.folders {
        items.push(Item {
            key: format!("{cache}/{}", folder.hierarchy.join("/")),
            kind: "folder".into(),
            title: folder.hierarchy.join("/"),
            count: count(folder.messages),
            counts: "messages".into(),
            within: Some(cache.clone()),
            chosen: false,
            note: None,
        });
    }
}

/// The reviewed profile `profile` names by its id or label, or the first one
/// that could be read.
fn readable_profile<'a>(
    scan: &'a migration_sources::thunderbird::Scan,
    profile: Option<&str>,
) -> Result<&'a migration_sources::thunderbird::ReviewedThunderbird> {
    if scan.reviewed.is_empty() {
        return if scan.problems.is_empty() {
            Err(anyhow!("no Thunderbird profile on this computer"))
        } else {
            Err(anyhow!(
                "no Thunderbird profile could be read: {}",
                scan.problems_text()
            ))
        };
    }
    match profile {
        Some(wanted) => scan
            .reviewed
            .iter()
            .find(|candidate| {
                candidate.profile.source_id == wanted || candidate.profile.label == wanted
            })
            .ok_or_else(|| {
                anyhow!(
                    "no Thunderbird profile {wanted:?} here; there are: {}",
                    scan.reviewed
                        .iter()
                        .map(|candidate| candidate.profile.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }),
        None => scan
            .reviewed
            .first()
            .ok_or_else(|| anyhow!("no Thunderbird profile on this computer")),
    }
}

/// What `tilo migrate choose` names an account by: its address, else its name.
fn account_key(account: &migration_sources::thunderbird::ThunderbirdAccount) -> String {
    account
        .email
        .clone()
        .filter(|email| !email.is_empty())
        .unwrap_or_else(|| account.display_name.clone())
}

/// What a run of `review` would bring, for its History entry: `ada@example.com: 4 messages,
/// 2 contacts`.
#[must_use]
pub fn summary(review: &Review) -> String {
    let accounts: Vec<&str> = review
        .chosen()
        .into_iter()
        .filter(|item| item.kind == "account")
        .map(|item| item.key.as_str())
        .collect();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for item in review.chosen() {
        if item.kind != "account" && item.kind != CACHE_KIND {
            let total = counts.entry(item.counts.as_str()).or_default();
            *total = total.saturating_add(item.count);
        }
    }
    let counted: Vec<String> = counts
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(what, count)| super::services::counted(*count, what))
        .collect();
    let mut summary = String::from("From Thunderbird");
    if !accounts.is_empty() {
        summary.push_str(": connect ");
        summary.push_str(&accounts.join(", "));
    }
    if !counted.is_empty() {
        summary.push_str(" (");
        summary.push_str(&counted.join(", "));
        summary.push(')');
    }
    summary
}

/// Bring what `review` chose over, landing through `lander`.
///
/// # Errors
/// The profile could not be read, the person said no to connecting an account, or the server or
/// the workspace refused; what landed before stays landed.
pub fn land(home: &Path, review: &Review, lander: &mut Lander<'_>) -> Result<()> {
    let profiles = migration_sources::thunderbird::discover(home).map_err(|why| anyhow!(why))?;
    let profile = profiles
        .iter()
        .find(|profile| profile.source_id == review.handle)
        .ok_or_else(|| anyhow!("the reviewed Thunderbird profile is no longer here"))?;
    let chosen: std::collections::BTreeSet<&str> = review
        .chosen()
        .into_iter()
        .map(|item| item.key.as_str())
        .collect();
    let mut saved: Option<Vec<migration_sources::thunderbird::SavedLogin>> = None;
    for account in &profile.accounts {
        let key = account_key(account);
        if !chosen.contains(key.as_str()) {
            continue;
        }
        if saved.is_none() {
            saved = Some(
                match migration_sources::thunderbird::saved_logins(home, &profile.source_id) {
                    Ok(logins) => logins,
                    Err(migration_sources::thunderbird::LoginsError::NoneSaved) => Vec::new(),
                    Err(why) => {
                        lander.say(&format!(
                            "{why}: each account is connected without its password, and Mail \
                             asks for it once"
                        ));
                        Vec::new()
                    }
                },
            );
        }
        connect_account(account, saved.as_deref().unwrap_or_default(), lander)?;
    }
    for account in &profile.accounts {
        let address = account
            .email
            .clone()
            .unwrap_or_else(|| account.display_name.clone());
        let cache = format!("{CACHE}{}", account_key(account));
        for folder in &account.folders {
            let folder_key = format!("{cache}/{}", folder.hierarchy.join("/"));
            if !chosen.contains(folder_key.as_str()) {
                continue;
            }
            land_folder(home, profile, &address, folder, lander)?;
        }
    }
    for book in &profile.address_books {
        if !chosen.contains(format!("contacts/{}", book.name).as_str()) {
            continue;
        }
        lander.open("contacts");
        let contacts =
            migration_sources::thunderbird::contacts(home, &profile.source_id, &book.source_id)
                .map_err(|why| anyhow!(why))?;
        for batch in contacts.chunks(CONTACTS_PER_ACTION) {
            lander.batch(
                batch,
                |contact| format!("contact:{}", text(contact, "id")),
                "contacts",
                |workspace, run, contacts| {
                    workspace.act(run, ADD_CONTACTS, json!({"contacts": contacts}))?;
                    Ok(())
                },
            )?;
        }
    }
    for calendar in &profile.calendars {
        if !chosen.contains(format!("calendar/{}", calendar.name).as_str()) {
            continue;
        }
        lander.open("calendar");
        let events =
            migration_sources::thunderbird::events(home, &profile.source_id, &calendar.source_id)
                .map_err(|why| anyhow!(why))?;
        for event in events {
            let key = format!("event:{}", text(&event, "id"));
            lander.once(&key, "events", |workspace, run| {
                workspace.act(run, NEW_EVENT, json!({"event": event}))?;
                Ok(Landed::One)
            })?;
        }
    }
    Ok(())
}

/// Connect `account`: shown to the person, and once they say yes, its servers, the other names
/// and addresses it sends as, and the password Thunderbird saved for it go to the server that
/// keeps their workspace, which signs in with it before keeping anything. Whether it is
/// connected is what that server says it keeps, never what an earlier migration noted: an account it
/// keeps already is told as such, and one it does not is connected again.
///
/// # Errors
/// The account has no server settings Mail can use, the person said no, or the server refused
/// (its mail server did not take the password, say), in its words.
fn connect_account(
    account: &migration_sources::thunderbird::ThunderbirdAccount,
    logins: &[migration_sources::thunderbird::SavedLogin],
    lander: &mut Lander<'_>,
) -> Result<()> {
    if let Some(provider) = signs_in_with(account) {
        return through_provider(account, provider, lander);
    }
    let Some(adding) = account_arguments(account, logins) else {
        bail!(
            "{} has no server Mail can sync from (a local or POP account): copy its messages \
             instead (tilo migrate choose thunderbird {CACHE}{})",
            account.display_name,
            account_key(account)
        );
    };
    connect_mail(adding, lander)
}

/// An account that signs in with its provider's own sign-in (`provider`: Google, Microsoft):
/// connected only by that sign-in, which the person gives in Settings, never with the token
/// Thunderbird keeps, which Thunderbird's own sign-in made for Thunderbird alone. Told as such,
/// and noted nowhere, so a later migration looks again; one the server lists already is.
fn through_provider(
    account: &migration_sources::thunderbird::ThunderbirdAccount,
    provider: &str,
    lander: &mut Lander<'_>,
) -> Result<()> {
    let address = account_key(account);
    let kept = lander.person(MAIL_ACCOUNTS, "list", json!({}))?;
    if keeps(&kept, &address) {
        lander.say(&format!("{address} is connected already"));
        return lander.noted(
            &format!("connected:{address}"),
            "accounts",
            &Landed::Already,
        );
    }
    lander.say(&format!(
        "{address} is not connected: it signs in with {provider}'s own sign-in. Connect it in \
         Settings > External accounts with {provider}; Mail then syncs it from its server."
    ));
    Ok(())
}

/// The provider whose own sign-in `account` uses (Google, Microsoft), when Thunderbird signs in
/// to it that way rather than with a password.
fn signs_in_with(
    account: &migration_sources::thunderbird::ThunderbirdAccount,
) -> Option<&'static str> {
    let candidate = account.settings.as_ref()?.candidate.as_ref()?;
    candidate
        .requires_oauth()
        .then(|| match candidate.provider() {
            Some("google") => "Google",
            Some("microsoft") => "Microsoft",
            _ => "its mail provider",
        })
}

/// Connect the mail account `adding` describes (as `person/mail-accounts#add` takes it), once
/// the person says yes, and only say so once the server lists it.
///
/// # Errors
/// The person said no, or the server refused, in its words.
pub fn connect_mail(adding: Value, lander: &mut Lander<'_>) -> Result<()> {
    let address = text(&adding, "address").to_owned();
    let key = format!("connected:{address}");
    let kept = lander.person(MAIL_ACCOUNTS, "list", json!({}))?;
    if keeps(&kept, &address) {
        lander.say(&format!("{address} is connected already"));
        return lander.noted(&key, "accounts", &Landed::Already);
    }
    let saved = adding.get("password").is_some();
    let question = connect_question(&adding, saved);
    if !lander.confirm(&question)? {
        bail!("{address} was not connected: you said no");
    }
    let added = lander
        .person(MAIL_ACCOUNTS, "add", adding)
        .map_err(|error| error.context(format!("{address} was not connected")))?;
    // What the server answers is the account it keeps: anything else is not one.
    if !keeps(&json!({"accounts": [added]}), &address) {
        bail!("{address} was not connected: the server answered no account for it");
    }
    let then = lander.person(MAIL_ACCOUNTS, "list", json!({}))?;
    if !keeps(&then, &address) {
        bail!("{address} was not connected: the server does not list it");
    }
    lander.say(&if saved {
        format!("{address} is connected: Mail syncs it from its server now")
    } else {
        format!(
            "{address} is connected without a password, as Thunderbird saved none: Mail asks \
             for it once, then syncs the account from its server"
        )
    });
    lander.noted(&key, "accounts", &Landed::One)
}

/// Whether the server's `listing` of the person's mail accounts holds `address`.
fn keeps(listing: &Value, address: &str) -> bool {
    listing
        .get("accounts")
        .and_then(Value::as_array)
        .is_some_and(|accounts| {
            accounts.iter().any(|account| {
                account
                    .get("address")
                    .and_then(Value::as_str)
                    .is_some_and(|kept| kept.eq_ignore_ascii_case(address))
            })
        })
}

/// What the person is asked before `adding` is connected: the account, its servers, the
/// addresses it sends as, and whether its saved password goes with it. Never the password.
fn connect_question(adding: &Value, saved: bool) -> String {
    let mut question = format!(
        "Connect {} to Mail: receives from {}:{}, sends by {}:{}",
        text(adding, "address"),
        text(adding, "incomingHost"),
        adding["incomingPort"],
        text(adding, "outgoingHost"),
        adding["outgoingPort"],
    );
    if let Some(identities) = adding.get("identities").and_then(Value::as_array) {
        let names: Vec<&str> = identities.iter().filter_map(Value::as_str).collect();
        if !names.is_empty() {
            question.push_str(", also sends as ");
            question.push_str(&names.join(", "));
        }
    }
    question.push_str(if saved {
        ", with the password Thunderbird saved, sealed in your vault?"
    } else {
        "? Thunderbird saved no password: Mail asks for it once."
    });
    question
}

/// An account's incoming and outgoing servers, as Mail would reach them.
struct Servers {
    imap_host: String,
    imap_port: u16,
    smtp_host: String,
    smtp_port: u16,
}

/// The servers of `account`, when it is one Mail syncs from a server (IMAP), not a local or
/// POP one.
fn servers(account: &migration_sources::thunderbird::ThunderbirdAccount) -> Option<Servers> {
    let config = config(account)?;
    Some(Servers {
        imap_host: config.imap_host,
        imap_port: config.imap_port,
        smtp_host: config.smtp_host,
        smtp_port: config.smtp_port,
    })
}

/// The settings Mail would keep for `account`, checked: `None` for an account Mail cannot reach
/// (local folders, POP) or whose settings do not check.
fn config(
    account: &migration_sources::thunderbird::ThunderbirdAccount,
) -> Option<accounts::email::AccountConfig> {
    use accounts::email::AuthMethod;
    let candidate = account.settings.as_ref()?.candidate.as_ref()?;
    candidate
        .reviewed_config(AuthMethod::Password)
        .or_else(|_| candidate.reviewed_config(AuthMethod::OAuth))
        .ok()
}

/// What the person's `person/mail-accounts#add` takes for `account`, including the password
/// Thunderbird saved for it: `None` for an account Mail cannot reach (local folders, POP) or
/// whose settings do not check.
fn account_arguments(
    account: &migration_sources::thunderbird::ThunderbirdAccount,
    logins: &[migration_sources::thunderbird::SavedLogin],
) -> Option<Value> {
    use accounts::email::AuthMethod;
    let config = config(account)?;
    let password = (config.auth == AuthMethod::Password)
        .then(|| {
            migration_sources::thunderbird::password_for(
                logins,
                "imap",
                &config.imap_host,
                &config.username,
            )
        })
        .flatten();
    let identities: Vec<String> = account
        .identities
        .iter()
        .filter(|identity| !identity.email.eq_ignore_ascii_case(&config.email))
        .map(|identity| {
            if identity.name.trim().is_empty() {
                identity.email.clone()
            } else {
                format!("{} <{}>", identity.name.trim(), identity.email)
            }
        })
        .collect();
    let name = account
        .identities
        .iter()
        .find(|identity| identity.email.eq_ignore_ascii_case(&config.email))
        .map(|identity| identity.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or(account.display_name.as_str());
    let mut arguments = json!({
        "address": config.email,
        "displayName": name,
        "incomingHost": config.imap_host,
        "incomingPort": config.imap_port,
        "outgoingHost": config.smtp_host,
        "outgoingPort": config.smtp_port,
    });
    if let Some(fields) = arguments.as_object_mut() {
        if !config.username.is_empty() && !config.username.eq_ignore_ascii_case(&config.email) {
            fields.insert("username".into(), json!(config.username));
        }
        if !identities.is_empty() {
            fields.insert("identities".into(), json!(identities));
        }
        // The sign-in travels once, to the server that keeps the account, which seals it in the
        // person's vault; no store of this computer keeps it.
        if let Some(password) = password {
            fields.insert("password".into(), json!(password));
        }
    }
    Some(arguments)
}

/// Each message of `folder`, delivered to the workspace's Mail as the account received it.
fn land_folder(
    home: &Path,
    profile: &migration_sources::thunderbird::ThunderbirdProfile,
    address: &str,
    folder: &migration_sources::thunderbird::ThunderbirdFolder,
    lander: &mut Lander<'_>,
) -> Result<()> {
    lander.open("mail");
    let place = mail_folder(folder);
    let mut failure = None;
    migration_sources::thunderbird::messages(
        home,
        &profile.source_id,
        &folder.source_id,
        &mut |message| match land_message(address, place, &message, lander) {
            Ok(()) => std::ops::ControlFlow::Continue(()),
            Err(error) => {
                failure = Some(error);
                std::ops::ControlFlow::Break(())
            }
        },
    )
    .map_err(|why| anyhow!(why))?;
    failure.map_or(Ok(()), Err)
}

/// A message as it was sent: without the `From ` line an mbox puts before each message to
/// separate it from the one before, which is the folder's and not the message's.
fn as_sent(stored: &[u8]) -> &[u8] {
    if !stored.starts_with(b"From ") {
        return stored;
    }
    stored
        .iter()
        .position(|byte| *byte == b'\n')
        .and_then(|end| stored.get(end.saturating_add(1)..))
        .unwrap_or_default()
}

fn land_message(
    address: &str,
    folder: &str,
    message: &migration_sources::thunderbird::StoredMessage,
    lander: &mut Lander<'_>,
) -> Result<()> {
    let Some(delivery) = delivery(&message.bytes, folder, message.read, address) else {
        return Ok(());
    };
    // The message's own Message-ID names it; one without is named by what it says.
    let name = message
        .message_id
        .clone()
        .unwrap_or_else(|| blake3::hash(&message.bytes).to_hex().to_string());
    lander.once(
        &format!("message:{address}:{name}"),
        "messages",
        |workspace, run| {
            // The message as it was sent travels whole, so its HTML, attachments and headers
            // are what Mail keeps; the words beside it are for whoever reads the record alone.
            // Its large parts travel apart from it, each in pieces, so a message of any size
            // comes over and no one upload or piece of content holds all of it.
            let split = tilo_mail_parts::split(as_sent(&message.bytes));
            let original = workspace.put(&split.rest)?;
            let mut parts = Vec::with_capacity(split.apart.len());
            for part in &split.apart {
                let pieces = part
                    .pieces()
                    .map(|piece| workspace.put(piece))
                    .collect::<Result<Vec<_>>>()?;
                parts.push(json!({"at": part.at, "size": part.size, "pieces": pieces}));
            }
            let mut delivery = delivery;
            if let Some(fields) = delivery.as_object_mut() {
                fields.insert("original".into(), json!(original));
                if !parts.is_empty() {
                    fields.insert("parts".into(), json!(parts));
                }
                if let Some(id) = &message.message_id {
                    fields.insert("message_id".into(), json!(id));
                }
            }
            match workspace.act(run, DELIVER, delivery) {
                Ok(_) => Ok(Landed::One),
                // Mail holds it already: an earlier run, or another computer's, brought it.
                Err(error) if already_held(&error) => Ok(Landed::Already),
                Err(error) => Err(error),
            }
        },
    )
}

/// Whether `error` is the refusal that says Mail holds the message already.
fn already_held(error: &anyhow::Error) -> bool {
    super::workspace::Refused::of(error)
        .is_some_and(|refused| refused.code() == tilo_actions::codes::MESSAGE_ALREADY_HELD)
}

/// What Mail's `deliver-message` takes for the message `bytes`: who sent it to whom, its subject,
/// date and text, the folder it is in, whether it was read, and the conversation it belongs to.
/// `None` for bytes that are not a message.
#[must_use]
pub fn delivery(bytes: &[u8], folder: &str, read: bool, account: &str) -> Option<Value> {
    use mail_parser::{MessageParser, MimeHeaders as _};
    let parsed = MessageParser::default().parse(bytes)?;
    let addresses = |list: Option<&mail_parser::Address<'_>>| -> String {
        list.map(|list| {
            list.iter()
                .filter_map(|one| {
                    let address = one.address()?;
                    Some(match one.name() {
                        Some(name) if !name.trim().is_empty() => {
                            format!("{} <{address}>", name.trim())
                        }
                        _ => address.to_owned(),
                    })
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
    };
    let from = addresses(parsed.from());
    let from = if from.is_empty() {
        account.to_owned()
    } else {
        from
    };
    let to = addresses(parsed.to());
    let cc = addresses(parsed.cc());
    let subject = parsed.subject().unwrap_or_default().to_owned();
    let body = parsed
        .body_text(0)
        .map(std::borrow::Cow::into_owned)
        .or_else(|| parsed.body_html(0).map(|html| strip_tags(&html)))
        .unwrap_or_default();
    let date = parsed.date().map(mail_parser::DateTime::to_rfc3339);
    // A conversation is named by the first message it began with: what a reply references.
    let conversation = first_id(&header(bytes, "references").unwrap_or_default())
        .or_else(|| first_id(&header(bytes, "in-reply-to").unwrap_or_default()))
        .or_else(|| first_id(&header(bytes, "message-id").unwrap_or_default()));
    let mut delivery = json!({
        "from": from, "subject": subject, "body": body, "folder": folder, "read": read,
    });
    if let Some(fields) = delivery.as_object_mut() {
        if !to.is_empty() {
            fields.insert("to".into(), json!(to));
        }
        if !cc.is_empty() {
            fields.insert("cc".into(), json!(cc));
        }
        if let Some(date) = date {
            fields.insert("date".into(), json!(date));
        }
        if let Some(conversation) = conversation {
            fields.insert("conversation".into(), json!(conversation));
        }
        // Mail lists these before it holds the message's bytes.
        let attachments: Vec<Value> = parsed
            .attachments()
            .map(|part| {
                let media_type = part.content_type().map(|kind| {
                    format!("{}/{}", kind.ctype(), kind.subtype().unwrap_or_default())
                        .to_ascii_lowercase()
                });
                json!({
                    "filename": part.attachment_name().unwrap_or("attachment.bin"),
                    "media_type": media_type,
                    "size": part.contents().len(),
                })
            })
            .collect();
        if !attachments.is_empty() {
            fields.insert("attachments".into(), json!(attachments));
        }
    }
    Some(delivery)
}

/// The first `<id>` of a header naming messages.
fn first_id(text: &str) -> Option<String> {
    let (_, after) = text.split_once('<')?;
    let (id, _) = after.split_once('>')?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// The text of an HTML body, for a message that has no plain one.
fn strip_tags(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => text.push(c),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The folder Mail files a Thunderbird folder's messages in.
fn mail_folder(folder: &migration_sources::thunderbird::ThunderbirdFolder) -> &'static str {
    use migration_sources::thunderbird::FolderKind;
    match folder.kind {
        FolderKind::Inbox => "inbox",
        FolderKind::Sent => "sent",
        FolderKind::Drafts => "drafts",
        FolderKind::Trash => "trash",
        FolderKind::Junk => "junk",
        _ => "archive",
    }
}

/// The first `name:` header of a message, unfolded.
fn header(bytes: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(bytes.get(..bytes.len().min(64 * 1024))?);
    let mut found: Option<String> = None;
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        if let Some(value) = found.as_mut() {
            if line.starts_with([' ', '\t']) {
                value.push(' ');
                value.push_str(line.trim());
                continue;
            }
            break;
        }
        if let Some((field, value)) = line.split_once(':')
            && field.trim().eq_ignore_ascii_case(name)
        {
            found = Some(value.trim().to_owned());
        }
    }
    found
}

/// A count the reader made, as the review shows it; nothing counted reads as none.
fn count(counted: Option<u64>) -> usize {
    counted.map_or(0, |count| usize::try_from(count).unwrap_or(usize::MAX))
}

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::workspace::Refused;
    use super::*;

    #[test]
    fn mail_holding_a_message_is_told_by_its_code() {
        let held: anyhow::Error = Refused::coded(
            tilo_actions::codes::MESSAGE_ALREADY_HELD,
            "this message is in the mailbox already, as m1",
        )
        .into();
        assert!(already_held(&held));
        let other: anyhow::Error =
            Refused::coded("refused", "this message is in the mailbox already").into();
        assert!(
            !already_held(&other),
            "the app's words alone decide nothing"
        );
    }

    #[test]
    fn a_message_is_delivered_as_its_account_received_it() {
        let bytes =
            b"Message-ID: <r2@example.net>\r\nReferences: <r1@example.net> <r2a@example.net>\r\n\
From: Grace Hopper <grace@example.net>\r\nTo: Ada <ada@example.com>\r\nCc: team@example.com\r\n\
Subject: Re: Quarterly plan\r\nDate: Tue, 3 Nov 2026 10:00:00 +0000\r\n\r\nSounds good.\r\n";
        let delivery = delivery(bytes, "inbox", true, "ada@example.com").unwrap();
        assert_eq!(delivery["from"], "Grace Hopper <grace@example.net>");
        assert_eq!(delivery["to"], "Ada <ada@example.com>");
        assert_eq!(delivery["cc"], "team@example.com");
        assert_eq!(delivery["subject"], "Re: Quarterly plan");
        assert_eq!(delivery["date"], "2026-11-03T10:00:00Z");
        assert_eq!(delivery["folder"], "inbox");
        assert_eq!(delivery["read"], true);
        assert_eq!(delivery["conversation"], "r1@example.net");
        assert!(delivery["body"].as_str().unwrap().contains("Sounds good."));
    }

    #[test]
    fn an_html_only_message_keeps_its_words() {
        let bytes = b"From: a@example.net\r\nSubject: x\r\nContent-Type: text/html\r\n\r\n<p>Hello <b>there</b></p>";
        let delivery = delivery(bytes, "archive", false, "ada@example.com").unwrap();
        assert!(
            delivery["body"].as_str().unwrap().contains("Hello"),
            "{delivery}"
        );
    }
}
