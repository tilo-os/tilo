//! Thunderbird on this computer as a migration source.
//!
//! The profile is read where it is, by the same reader the Tilo OS installer's migration uses
//! ([`migration_sources::thunderbird`]), and never written. A review lists each account with
//! its folders, and the profile's address books and calendars; the person chooses an account
//! (or some of its folders), and the address books and calendars only if they choose them.
//!
//! What comes over lands through the apps' own actions as the agent's run: each message in the
//! workspace's Mail as it was sent, its MIME bytes whole (HTML, attachments, headers; its large
//! parts carried apart from the rest, so no message is too large to come over), in its
//! folder with its read state and its conversation (Mail's `deliver-message`, which keeps it
//! under the id its `Message-ID` names and knows a message it already holds), each contact and
//! event in
//! Contacts and Calendar under an id the source decides, so running it again names the same
//! records and adds nothing.
//!
//! A chosen account itself joins the person's accounts: its server settings, the other names
//! and addresses it sends as, and the password Thunderbird saved for it, through Mail's
//! `add-account`, which their tab puts to them before it runs. The password travels once, with
//! the account, into the store that keeps it — the person's vault on the host when the workspace
//! runs on a host, this computer's when Mail here keeps it — and no second vault keeps a copy. It is
//! never printed or answered. An account Thunderbird saved no password for asks the person for
//! it when Mail first signs in.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, anyhow};
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
/// Where an email account joins the person's accounts (Mail's, which the person allows).
const ADD_ACCOUNT: &str = "mail/mail.mailbox#add-account";

/// How many contacts one action carries.
const CONTACTS_PER_ACTION: usize = 100;

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
        let key = account_key(account);
        let messages = account
            .folders
            .iter()
            .map(|folder| count(folder.messages))
            .sum();
        items.push(Item {
            key: key.clone(),
            kind: "account".into(),
            title: format!("{} ({})", account.display_name, account.kind),
            count: messages,
            counts: "messages".into(),
            within: None,
            chosen: false,
        });
        for folder in &account.folders {
            items.push(Item {
                key: format!("{key}/{}", folder.hierarchy.join("/")),
                kind: "folder".into(),
                title: folder.hierarchy.join("/"),
                count: count(folder.messages),
                counts: "messages".into(),
                within: Some(key.clone()),
                chosen: false,
            });
        }
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
        summary.push_str(": ");
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
/// The profile could not be read or the workspace refused; what landed before stays landed.
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
                        (lander.progress)(&format!(
                            "{why}: the accounts wait for the person to sign them in, in Settings"
                        ));
                        Vec::new()
                    }
                },
            );
        }
        let logins = saved.as_deref().unwrap_or_default();
        land_account(account, logins, lander)?;
        let address = account
            .email
            .clone()
            .unwrap_or_else(|| account.display_name.clone());
        for folder in &account.folders {
            let folder_key = format!("{key}/{}", folder.hierarchy.join("/"));
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

/// The settings of `account` (its servers, the other names and addresses it sends as, and the
/// password Thunderbird saved for it) into the person's accounts, where that store keeps them.
/// An account with no server settings Mail can use (a local or POP one) keeps only its mail.
fn land_account(
    account: &migration_sources::thunderbird::ThunderbirdAccount,
    logins: &[migration_sources::thunderbird::SavedLogin],
    lander: &mut Lander<'_>,
) -> Result<()> {
    let Some(arguments) = account_arguments(account, logins) else {
        return Ok(());
    };
    lander.open("mail");
    lander.once(
        &format!("account:{}", account_key(account)),
        "accounts",
        |workspace, run| match workspace.act(run, ADD_ACCOUNT, arguments.clone()) {
            Ok(answer) if answer.said("already") => Ok(Landed::Already),
            Ok(_) => Ok(Landed::One),
            Err(error) => Err(mail_not_open(error, text(&arguments, "email"))),
        },
    )
}

/// Why an account could not be added, said as what to do: Mail adds it from the person's own
/// tab, so it has to be open there.
pub fn mail_not_open(error: anyhow::Error, email: &str) -> anyhow::Error {
    if super::workspace::Refused::of(&error)
        .is_some_and(|refused| refused.code() == "unknown_action")
    {
        return error.context(format!(
            "{email} is added to the person's accounts by Mail in their tab, and Mail is not open there: ask them to open Mail, then start this again"
        ));
    }
    error
}

/// What Mail's `add-account` takes for `account`, including the password Thunderbird saved
/// for it: `None` for an account Mail cannot reach (local folders, POP) or whose settings do
/// not check.
fn account_arguments(
    account: &migration_sources::thunderbird::ThunderbirdAccount,
    logins: &[migration_sources::thunderbird::SavedLogin],
) -> Option<Value> {
    use accounts::email::AuthMethod;
    let candidate = account.settings.as_ref()?.candidate.as_ref()?;
    let config = candidate
        .reviewed_config(AuthMethod::Password)
        .or_else(|_| candidate.reviewed_config(AuthMethod::OAuth))
        .ok()?;
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
        "display_name": name,
        "email": config.email,
        "username": config.username,
        "imap_host": config.imap_host,
        "imap_port": config.imap_port,
        "smtp_host": config.smtp_host,
        "smtp_port": config.smtp_port,
    });
    if let Some(fields) = arguments.as_object_mut() {
        if !identities.is_empty() {
            fields.insert("identities".into(), json!(identities));
        }
        // The sign-in travels with the account into the store that keeps it
        // (the person's vault on the host when it is kept there), and is never sealed
        // into a vault of this computer's beside it.
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
            let split = tilo_mail_parts::split(&message.bytes);
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
    use mail_parser::MessageParser;
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
    fn a_mail_that_does_not_run_says_what_to_do() {
        let unopened: anyhow::Error = Refused::coded(
            "unknown_action",
            "no action addressed by (mail, add-account)",
        )
        .into();
        let said = format!("{:#}", mail_not_open(unopened, "ada@example.com"));
        assert!(said.contains("Mail is not open there"), "{said}");
        let refused: anyhow::Error =
            Refused::coded("refused", "the account's servers do not check").into();
        let said = format!("{:#}", mail_not_open(refused, "ada@example.com"));
        assert_eq!(said, "the account's servers do not check");
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
