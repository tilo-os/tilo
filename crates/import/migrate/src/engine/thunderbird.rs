//! Thunderbird on this computer as a migration source.
//!
//! The profile is read where it is, by the same reader the Tilo OS installer's migration uses
//! ([`migration_sources::thunderbird`]), and never written: the stores it keeps in databases
//! (its saved sign-ins' key, address books, calendars) are read from a private copy.
//!
//! Each account Thunderbird reads from a server is **connected**: its incoming and outgoing
//! servers, the names and addresses it sends as, and the password Thunderbird saved for it go to
//! the server that keeps the person's workspace, as their own request
//! (`workspace/mail-accounts#add`). That server signs in with the password first, and only once
//! the mail server took it keeps the account and seals the password in the person's vault. Mail
//! then syncs the account from its server, as it does any account. Nothing is copied from
//! Thunderbird's cache: the person's mail stays on their mail server, where Mail reads it.
//! Connecting is the person's to say: the migration shows each account and its servers at their
//! terminal and connects it once they say yes (or said `--yes` beforehand). The password is never
//! printed, logged or kept anywhere on this computer.
//!
//! Contacts and events land in Contacts and Calendar under an id the source decides, so running
//! it again names the same contacts and events and adds nothing.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::state::{Item, Review};
use super::{Landed, Lander};

/// The source name `tilo migrate` uses.
pub const SOURCE: &str = "thunderbird";

const IMPORT_VCARDS: &str = "contacts#import_vcard";
const NEW_EVENT: &str = "calendar/calendar.calendar#new-event";
/// The person's mail accounts on the server that keeps their workspace, which only they add to.
pub const MAIL_ACCOUNTS: &str = "workspace/mail-accounts";

/// The review of the Thunderbird profile at `home`: `profile` names one when there are
/// several; the default profile otherwise. Nothing is chosen until the person says.
///
/// # Errors
/// No Thunderbird profile is here, or none that is here could be read; the
/// message names what could not be read and why.
pub fn review(home: &Path, profile: Option<&str>) -> Result<Review> {
    let scan = migration_sources::thunderbird::scan(home);
    let reviewed = readable_profile(&scan, profile)?;
    let found = reviewed.profile();
    let mut items = Vec::new();
    for account in &found.accounts {
        account_items(account, &mut items);
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

/// What `account` offers: connecting it, when Mail can sync it from its server.
fn account_items(
    account: &migration_sources::thunderbird::ThunderbirdAccount,
    items: &mut Vec<Item>,
) {
    let key = account_key(account);
    let servers = servers(account);
    let provider = signs_in_with(account);
    let title = match (provider, &servers) {
        (Some(provider), _) => Some(format!(
            "connect {} with {provider}'s sign-in, in Settings > Accounts; the \
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

/// What a run of `review` would bring, for its History entry: `ada@example.com: 2 contacts`.
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
        if item.kind != "account" {
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
    for book in &profile.address_books {
        if !chosen.contains(format!("contacts/{}", book.name).as_str()) {
            continue;
        }
        lander.open("contacts");
        let cards =
            migration_sources::thunderbird::address_book(home, &profile.source_id, &book.source_id)
                .map_err(|why| anyhow!(why))?;
        let key = format!("contacts/{}", book.name);
        lander.once(&key, "contacts", |workspace, run| {
            // The book goes over as the file every address book reads: Contacts imports its
            // cards, so nothing here decides what a card means.
            let content = workspace.put(cards.as_bytes())?;
            workspace.act(
                run,
                IMPORT_VCARDS,
                json!({"file": {
                    "content": content,
                    "name": format!("{}.vcf", book.name),
                    "media": "text/vcard",
                }}),
            )?;
            Ok(Landed::One)
        })?;
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
            "{} has no server Mail can sync from (a local or POP account), so it is not \
             connected: its mail stays in Thunderbird",
            account.display_name
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
         Settings > Accounts with {provider}; Mail then syncs it from its server."
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

/// Connect the mail account `adding` describes (as `workspace/mail-accounts#add` takes it), once
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

/// What `workspace/mail-accounts#add` takes for `account`, including the password
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

/// A count the reader made, as the review shows it; nothing counted reads as none.
fn count(counted: Option<u64>) -> usize {
    counted.map_or(0, |count| usize::try_from(count).unwrap_or(usize::MAX))
}

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}
