//! Mail the room holds whole, for whoever works without a mail server.
//!
//! A server's mail stays outside the mailbox (see [`crate::resource`]); the
//! mailbox holds only what the reader did with it. An agent's confined
//! worker, a seeded scenario or an injected event has no account and no
//! network, so mail delivered to the room itself is kept in the record
//! ([`HeldMessage`], under [`Mail::held`]): it replicates, replays and
//! undoes like every other mailbox edit, and every participant — a person's
//! window, an agent's headless replica — reads the same messages. Each one's
//! folder, read and starred state and labels are its ordinary
//! [`MessageState`], so archiving, starring or trashing it is the same verb
//! as for fetched mail; it has no server copy, so nothing is queued for a
//! server and nothing is ever sent from here.
//!
//! What is read from it — the mailbox's `read`, `search-messages`,
//! `export-content` — is projected from the record alone. Drafting a reply
//! writes the mailbox's one draft, which a person reviews and sends.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, ActionCx, Id, JsonSchema, Model, Refusal, Set};

use crate::actions::{DraftData, bounded_text};
use crate::model::{Addr, Folder, Message, MessageId, normalize_subject};
use crate::resource::{
    MAX_DRAFT_ADDRESS_CHARS, MAX_DRAFT_BODY_CHARS, MAX_DRAFT_SUBJECT_CHARS, Mail, MessageState,
};

/// The most characters a `Message-ID` or conversation key holds.
const MAX_KEY_CHARS: usize = 256;

/// How much of a body a listing shows.
const SNIPPET_CHARS: usize = 160;

/// The most messages a listing or a search answers, newest first.
const MOST_LISTED: usize = 50;

/// The most messages one export holds.
const MOST_EXPORTED: usize = 500;

/// One message stored whole in the mailbox.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = HeldMessage::valid)]
pub struct HeldMessage {
    /// Its RFC 5322 `Message-ID`, without angle brackets
    pub message_id: String,
    /// The sender, `Name <address>` or an address
    pub from: String,
    /// Recipients, comma-separated
    pub to: String,
    /// Copy recipients, comma-separated
    pub cc: String,
    /// Subject
    #[model(label)]
    pub subject: String,
    /// The message as plain text
    pub body: String,
    /// When it was sent, in minutes since the Unix epoch
    pub minutes: i64,
    /// The `Message-ID` of the message it answers
    pub in_reply_to: Option<String>,
    /// The conversation it belongs to: the `Message-ID` of the
    /// conversation's first message, or a key its author gave
    pub conversation: String,
}

impl HeldMessage {
    fn valid(&self) -> Result<(), Refusal> {
        let within = |text: &str, max: usize| text.chars().count() <= max;
        if !within(&self.from, MAX_DRAFT_ADDRESS_CHARS)
            || !within(&self.to, MAX_DRAFT_ADDRESS_CHARS)
            || !within(&self.cc, MAX_DRAFT_ADDRESS_CHARS)
            || !within(&self.subject, MAX_DRAFT_SUBJECT_CHARS)
            || !within(&self.body, MAX_DRAFT_BODY_CHARS)
            || !within(&self.message_id, MAX_KEY_CHARS)
            || !within(&self.conversation, MAX_KEY_CHARS)
        {
            return Err(Refusal::new("the message is too long"));
        }
        if self.from.trim().is_empty() || self.message_id.is_empty() {
            return Err(Refusal::new("a message has a sender and a Message-ID"));
        }
        Ok(())
    }

    /// The sender as an address.
    fn sender(&self) -> Addr {
        crate::transport::parse_addr_list(&self.from)
            .into_iter()
            .next()
            .unwrap_or_else(|| Addr::bare(self.from.clone()))
    }

    /// When it was sent, RFC 3339 in UTC.
    fn date(&self) -> String {
        mail_parser::DateTime::from_timestamp(self.minutes.saturating_mul(60)).to_rfc3339()
    }

    /// The first line of the body a reader sees, at most
    /// [`SNIPPET_CHARS`].
    fn snippet(&self) -> String {
        let line = self
            .body
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('>'))
            .unwrap_or("");
        let words = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if words.chars().count() > SNIPPET_CHARS {
            let cut: String = words.chars().take(SNIPPET_CHARS).collect();
            format!("{cut}…")
        } else {
            words
        }
    }

    /// The message as the window's mailbox shows one, in `state`'s folder.
    fn shown(&self, id: MessageId, state: &MessageState) -> Message {
        Message {
            id,
            rfc_message_id: Some(self.message_id.clone()),
            in_reply_to: self.in_reply_to.clone(),
            from: self.sender(),
            to: crate::transport::parse_addr_list(&self.to),
            cc: crate::transport::parse_addr_list(&self.cc),
            subject: self.subject.clone(),
            body: self.body.clone(),
            html_body: None,
            attachments: Vec::new(),
            timestamp_min: self.minutes,
            read: state.read,
            starred: state.starred,
            folder: state.folder,
            labels: state.labels.iter().cloned().collect(),
            account: String::new(),
            server: None,
        }
    }
}

/// A message delivered to the room: the parameters of
/// [`Mail::deliver_message`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Delivery {
    /// The sender, `Name <address>` or an address
    pub from: String,
    /// Recipients, comma-separated
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Copy recipients, comma-separated
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<String>,
    /// The subject
    pub subject: String,
    /// The message, as plain text
    pub body: String,
    /// When it was sent, RFC 3339 (2026-10-12T09:30:00Z) or RFC 822; now
    /// when omitted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// The folder it lands in; the inbox when omitted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<Folder>,
    /// Whether it was read already; unread when omitted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<bool>,
    /// The id of a held message it answers, joining its conversation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// A key naming the conversation it belongs to; when omitted it joins
    /// the one it answers, else one with the same subject, else starts one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
}

/// The id a held message is kept under: derived from what it says, so
/// delivering the same message twice (a scenario replayed, an event seen
/// again) is recognised rather than kept twice.
fn derived_id(parts: &[&str]) -> MessageId {
    let mut hasher = fnv::FnvHasher::with_key(0x6d61_696c_2e68_656c);
    let mut second = fnv::FnvHasher::with_key(0x6c64_2e6d_6573_7361);
    for part in parts {
        std::hash::Hasher::write(&mut hasher, part.as_bytes());
        std::hash::Hasher::write_u8(&mut hasher, 0);
        std::hash::Hasher::write(&mut second, part.as_bytes());
        std::hash::Hasher::write_u8(&mut second, 0xff);
    }
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&std::hash::Hasher::finish(&hasher).to_be_bytes());
    bytes[8..].copy_from_slice(&std::hash::Hasher::finish(&second).to_be_bytes());
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

/// `date` as minutes since the Unix epoch.
fn minutes_of(date: &str) -> Result<i64, Refusal> {
    let date = date.trim();
    mail_parser::DateTime::parse_rfc3339(date)
        .or_else(|| mail_parser::DateTime::parse_rfc822(date))
        .map(|at| at.to_timestamp().div_euclid(60))
        .ok_or_else(|| {
            Refusal::new(format!(
                "`{date}` is not a date: give RFC 3339, as 2026-10-12T09:30:00Z"
            ))
        })
}

/// A key as stored: no angle brackets, no spaces, bounded.
fn bare_key(value: &str) -> String {
    bounded_text(value, MAX_KEY_CHARS, false)
        .trim_start_matches('<')
        .trim_end_matches('>')
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect()
}

/// The message id an item names: a bare id, `message:<id>`,
/// `mail.message:<id>` or a published path ending in one.
fn message_named(item: &str) -> Option<&str> {
    let last = item.rsplit('/').next().unwrap_or(item);
    let id = last
        .strip_prefix("mail.message:")
        .or_else(|| last.strip_prefix("message:"))
        .unwrap_or(last);
    id.parse::<MessageId>().is_ok().then_some(id)
}

/// The folder an item names: `folder:<key>`, `mail.folder:<key>` or a
/// published path ending in one.
fn folder_named(item: &str) -> Option<Folder> {
    let last = item.rsplit('/').next().unwrap_or(item);
    last.strip_prefix("mail.folder:")
        .or_else(|| last.strip_prefix("folder:"))
        .and_then(Folder::from_key)
}

/// `text` quoted as a reply quotes it: each line after `> `.
fn quoted(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Mail {
    /// Keep `delivery` as a held message in its folder; the id it is kept
    /// under. Refused when the same message is held already.
    pub(crate) fn deliver(&mut self, cx: &ActionCx, delivery: &Delivery) -> Result<Id, Refusal> {
        let from = bounded_text(&delivery.from, MAX_DRAFT_ADDRESS_CHARS, false);
        if from.is_empty() {
            return Err(Refusal::new("a message has a sender"));
        }
        let to = bounded_text(
            delivery.to.as_deref().unwrap_or(""),
            MAX_DRAFT_ADDRESS_CHARS,
            false,
        );
        let cc = bounded_text(
            delivery.cc.as_deref().unwrap_or(""),
            MAX_DRAFT_ADDRESS_CHARS,
            false,
        );
        let subject = bounded_text(&delivery.subject, MAX_DRAFT_SUBJECT_CHARS, false);
        let body = bounded_text(&delivery.body, MAX_DRAFT_BODY_CHARS, true);
        let minutes = match delivery.date.as_deref().filter(|d| !d.trim().is_empty()) {
            Some(date) => minutes_of(date)?,
            None => cx.now_ms().div_euclid(60_000),
        };
        let id = derived_id(&[&from, &to, &cc, &subject, &body, &minutes.to_string()]);
        let key = Id::from_raw(id.to_string());
        if self.held.contains(&key) || self.messages.contains(&key) {
            return Err(Refusal::new(format!(
                "this message is in the mailbox already, as {id}"
            )));
        }
        let answered = match delivery.reply_to.as_deref().map(str::trim) {
            Some(reply) if !reply.is_empty() => Some(
                self.held_message(reply)
                    .map(|(_, held)| held.clone())
                    .ok_or("the message it answers is not held in this mailbox")?,
            ),
            _ => None,
        };
        let message_id = format!("{id}@mail.tilo.local");
        let conversation = delivery
            .conversation
            .as_deref()
            .map(bare_key)
            .filter(|key| !key.is_empty())
            .or_else(|| answered.as_ref().map(|held| held.conversation.clone()))
            .or_else(|| self.conversation_about(&subject))
            .unwrap_or_else(|| message_id.clone());
        self.held.push(
            key.clone(),
            HeldMessage {
                message_id,
                from,
                to,
                cc,
                subject,
                body,
                minutes,
                in_reply_to: answered.map(|held| held.message_id),
                conversation,
            },
        );
        self.messages.push(
            key.clone(),
            MessageState {
                read: delivery.read.unwrap_or(false),
                starred: false,
                folder: delivery.folder.unwrap_or_default(),
                labels: Set::new(),
                server: None,
                purged: false,
            },
        );
        Ok(key)
    }

    /// The conversation of a held message about `subject` (its reply and
    /// forward markers aside), when one is.
    fn conversation_about(&self, subject: &str) -> Option<String> {
        let topic = normalize_subject(subject);
        if topic.is_empty() {
            return None;
        }
        self.held
            .iter()
            .find(|(_, held)| normalize_subject(&held.subject).eq_ignore_ascii_case(&topic))
            .map(|(_, held)| held.conversation.clone())
    }

    /// The held message `item` names, unless it was deleted for good.
    pub(crate) fn held_message(&self, item: &str) -> Option<(&Id, &HeldMessage)> {
        let id = message_named(item.trim())?;
        let key = Id::from_raw(id);
        self.message(id)?;
        self.held.iter().find(|(held, _)| **held == key)
    }

    /// The held messages not deleted for good, newest first, with their
    /// state.
    fn held_newest(&self) -> Vec<(&Id, &HeldMessage, &MessageState)> {
        let mut held: Vec<_> = self
            .held
            .iter()
            .filter_map(|(id, held)| Some((id, held, self.message(id.as_str())?)))
            .collect();
        held.sort_by(|a, b| {
            b.1.minutes
                .cmp(&a.1.minutes)
                .then_with(|| a.0.as_str().cmp(b.0.as_str()))
        });
        held
    }

    /// Write the mailbox's draft to `to` about `subject` saying `body`,
    /// as a person would in the composer. A draft this wrote that nobody
    /// changed since is written over; any other is the work of the person
    /// writing it, and is left alone. The refusal names no other verb: an
    /// agent may neither save over a draft nor discard it without asking,
    /// since both reach outside the mailbox.
    pub(crate) fn draft_held(
        &mut self,
        to: &str,
        cc: &str,
        subject: &str,
        body: &str,
    ) -> Result<(), Refusal> {
        if self.has_draft() && !self.draft.untouched_since_drafted() {
            return Err(Refusal::new(
                "a person is writing a draft here, and it is theirs: leave it, or ask them to \
                 send or discard it",
            ));
        }
        let to = bounded_text(to, MAX_DRAFT_ADDRESS_CHARS, false);
        if crate::transport::parse_addr_list(&to).is_empty() {
            return Err(Refusal::new("a draft goes to at least one address"));
        }
        let body = bounded_text(body, MAX_DRAFT_BODY_CHARS, true);
        let started = crate::resource::started_body(&body, None);
        self.start_draft_body(body.clone(), None);
        self.save_draft(DraftData {
            to,
            cc: bounded_text(cc, MAX_DRAFT_ADDRESS_CHARS, false),
            subject: bounded_text(subject, MAX_DRAFT_SUBJECT_CHARS, false),
            body,
            ..DraftData::default()
        });
        let drafted = self.draft.fingerprint(&tilo_content::hash_bytes(&started));
        self.draft.drafted = Some(drafted);
        Ok(())
    }

    /// Draft a reply to the held message `message` saying `body` above the
    /// original, quoted; `all` copies everyone else it went to.
    pub(crate) fn draft_reply_held(
        &mut self,
        message: &str,
        body: &str,
        all: bool,
    ) -> Result<(), Refusal> {
        let (_, held) = self.held_message(message).ok_or(
            "no such message held in this mailbox: read the mailbox for the messages it holds",
        )?;
        let held = held.clone();
        let subject = if normalize_subject(&held.subject) == held.subject.trim() {
            format!("Re: {}", held.subject.trim())
        } else {
            held.subject.trim().to_owned()
        };
        let cc = if all {
            let sender = held.sender().email.to_ascii_lowercase();
            crate::transport::parse_addr_list(&format!("{}, {}", held.to, held.cc))
                .into_iter()
                .filter(|addr| addr.email.to_ascii_lowercase() != sender)
                .map(|addr| addr.rfc5322())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            String::new()
        };
        let text = format!(
            "{}\n\nOn {}, {} wrote:\n{}",
            body.trim_end(),
            held.date(),
            held.from,
            quoted(&held.body)
        );
        self.draft_held(&held.from, &cc, &subject, &text)
    }

    /// What the mailbox holds, for a caller with no screen: with no item,
    /// its folders, the held messages newest first and the draft; with a
    /// folder, the messages in it; with a message, the whole message and
    /// its conversation, oldest first.
    pub(crate) fn read_held(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        let item = item.map_or("", |item| item.as_str().trim());
        if item.is_empty() {
            return Ok(serde_json::json!({
                "folders": Folder::ALL
                    .iter()
                    .map(|folder| serde_json::json!({
                        "item": format!("mail.folder:{}", folder.as_key()),
                        "name": folder.title(),
                        "messages": self.held_newest().iter().filter(|(_, _, state)| state.folder == *folder).count(),
                    }))
                    .collect::<Vec<_>>(),
                "messages": self.listing(None),
                "draft": self.has_draft().then(|| self.draft_summary()),
            }));
        }
        if let Some(folder) = folder_named(item) {
            return Ok(serde_json::json!({
                "folder": folder.as_key(),
                "messages": self.listing(Some(folder)),
            }));
        }
        if item.ends_with("draft") {
            return Ok(serde_json::json!({ "draft": self.draft_summary() }));
        }
        let (id, held) = self.held_message(item).ok_or_else(|| {
            format!("no message `{item}` is held here; read the mailbox for the messages it holds")
        })?;
        let mut conversation: Vec<_> = self
            .held_newest()
            .into_iter()
            .filter(|(_, other, _)| other.conversation == held.conversation)
            .collect();
        conversation.reverse();
        Ok(serde_json::json!({
            "message": self.whole(id, held),
            "conversation": conversation
                .iter()
                .map(|(id, held, state)| line(id, held, state))
                .collect::<Vec<_>>(),
        }))
    }

    fn listing(&self, folder: Option<Folder>) -> Vec<serde_json::Value> {
        self.held_newest()
            .into_iter()
            .filter(|(_, _, state)| folder.is_none_or(|folder| state.folder == folder))
            .take(MOST_LISTED)
            .map(|(id, held, state)| line(id, held, state))
            .collect()
    }

    fn whole(&self, id: &Id, held: &HeldMessage) -> serde_json::Value {
        let state = self.message(id.as_str());
        serde_json::json!({
            "id": id.as_str(),
            "item": format!("mail.message:{}", id.as_str()),
            "message_id": held.message_id,
            "from": held.from,
            "to": held.to,
            "cc": held.cc,
            "subject": held.subject,
            "date": held.date(),
            "folder": state.map(|state| state.folder.as_key()),
            "read": state.is_some_and(|state| state.read),
            "starred": state.is_some_and(|state| state.starred),
            "in_reply_to": held.in_reply_to,
            "conversation": held.conversation,
            "body": held.body,
        })
    }

    fn draft_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "to": self.draft.to,
            "cc": self.draft.cc,
            "subject": self.draft.subject,
            "body": self.draft.body,
        })
    }

    /// The held messages whose sender, recipients, subject or body hold
    /// every word of `query` (ignoring case), newest first, in `folder`
    /// when one is named.
    pub(crate) fn search_held(
        &self,
        query: &str,
        folder: Option<Folder>,
    ) -> Result<serde_json::Value, String> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(str::to_lowercase)
            .filter(|word| !word.is_empty())
            .collect();
        if words.is_empty() {
            return Err("give words to search for".into());
        }
        let found: Vec<serde_json::Value> = self
            .held_newest()
            .into_iter()
            .filter(|(_, _, state)| folder.is_none_or(|folder| state.folder == folder))
            .filter(|(_, held, _)| {
                let text = format!(
                    "{}\n{}\n{}\n{}\n{}",
                    held.from, held.to, held.cc, held.subject, held.body
                )
                .to_lowercase();
                words.iter().all(|word| text.contains(word.as_str()))
            })
            .take(MOST_LISTED)
            .map(|(id, held, state)| line(id, held, state))
            .collect();
        Ok(serde_json::json!({ "query": query.trim(), "messages": found }))
    }

    /// Keep held mail as a file in the window's content store: one message
    /// (as Markdown, JSON or `.eml`), or a folder's or every message (as
    /// Markdown or JSON), newest first. Answers its address.
    pub(crate) fn export_held(
        &self,
        format: Option<&str>,
        message: Option<&str>,
        folder: Option<Folder>,
    ) -> Result<serde_json::Value, String> {
        let format = format.map_or("md", str::trim).to_ascii_lowercase();
        let chosen: Vec<(&Id, &HeldMessage)> = match message.filter(|m| !m.trim().is_empty()) {
            Some(message) => vec![self.held_message(message).ok_or(
                "no such message held in this mailbox: read the mailbox for the messages it holds",
            )?],
            None => self
                .held_newest()
                .into_iter()
                .filter(|(_, _, state)| folder.is_none_or(|folder| state.folder == folder))
                .take(MOST_EXPORTED)
                .map(|(id, held, _)| (id, held))
                .collect(),
        };
        // The draft being written is the mailbox's, not a held message: it is
        // exported with every message or the Drafts folder, marked a draft.
        let draft = self.written_draft().filter(|_| {
            message.is_none_or(|m| m.trim().is_empty())
                && folder.is_none_or(|folder| folder == Folder::Drafts)
        });
        if chosen.is_empty() && draft.is_none() {
            return Err("there is no held mail to export".into());
        }
        let exported = chosen.len() + usize::from(draft.is_some());
        let (bytes, media_type, extension) = match format.as_str() {
            "md" | "markdown" => {
                let mut text = markdown(&chosen);
                if let Some(draft) = &draft {
                    if !text.is_empty() {
                        text.push_str("\n---\n\n");
                    }
                    text.push_str(&draft_markdown(draft));
                }
                (text.into_bytes(), "text/markdown", "md")
            }
            "json" => {
                let mut all: Vec<_> = chosen
                    .iter()
                    .map(|(id, held)| self.whole(id, held))
                    .collect();
                if let Some(draft) = &draft {
                    all.push(serde_json::json!({
                        "draft": true,
                        "folder": Folder::Drafts.as_key(),
                        "to": draft.to,
                        "cc": draft.cc,
                        "bcc": draft.bcc,
                        "subject": draft.subject,
                        "body": draft.body,
                    }));
                }
                let text = serde_json::to_string_pretty(&all)
                    .map_err(|error| format!("the mail does not encode: {error}"))?;
                (text.into_bytes(), "application/json", "json")
            }
            "eml" => match chosen.as_slice() {
                [(_, held)] if draft.is_none() => (eml(held)?, "message/rfc822", "eml"),
                _ => {
                    return Err(
                        "an .eml file holds one message: name it, or export as md or json".into(),
                    );
                }
            },
            other => return Err(format!("`{other}` is not a format: md, json or eml")),
        };
        let store = tilo_resource::content::current()
            .ok_or("this window keeps no content to export into")?;
        let hash = store
            .put(&bytes)
            .map_err(|error| format!("the export could not be kept: {error}"))?;
        let content =
            tilo_actions::content_resource(&hash).ok_or("the export has no content address")?;
        Ok(serde_json::json!({
            "content": content,
            "format": extension,
            "media_type": media_type,
            "messages": exported,
            "drafts": usize::from(draft.is_some()),
            "bytes": bytes.len(),
        }))
    }

    /// The mailbox's draft as it is written now — its body the text being
    /// written when it was not saved yet — or `None` when nothing is in it.
    fn written_draft(&self) -> Option<WrittenDraft> {
        let draft = &self.draft;
        let body = if draft.body.trim().is_empty() {
            draft
                .rich_body
                .as_ref()
                .map(tilo_document::RichText::text)
                .unwrap_or_default()
        } else {
            draft.body.clone()
        };
        let written = [&draft.to, &draft.cc, &draft.subject, &body]
            .iter()
            .any(|field| !field.trim().is_empty());
        written.then(|| WrittenDraft {
            to: draft.to.clone(),
            cc: draft.cc.clone(),
            bcc: draft.bcc.clone(),
            subject: draft.subject.clone(),
            body,
        })
    }

    /// Show the held mail in the window's mailbox, beside what its servers
    /// hold. Only what the mailbox lacks is added, so an unchanged mailbox
    /// keeps its revision; their flags are applied after, as for any mail.
    pub(crate) fn show_held(&self, mailbox: &mut crate::model::Mailbox) {
        for (id, held) in self.held.iter() {
            let (Ok(message_id), Some(state)) = (
                id.as_str().parse::<MessageId>(),
                self.messages.get(id).filter(|state| !state.purged),
            ) else {
                continue;
            };
            if mailbox.get(message_id).is_none() {
                mailbox.insert(held.shown(message_id, state));
            }
        }
    }
}

/// One message as a listing shows it.
fn line(id: &Id, held: &HeldMessage, state: &MessageState) -> serde_json::Value {
    serde_json::json!({
        "id": id.as_str(),
        "item": format!("mail.message:{}", id.as_str()),
        "from": held.from,
        "to": held.to,
        "subject": held.subject,
        "date": held.date(),
        "folder": state.folder.as_key(),
        "read": state.read,
        "conversation": held.conversation,
        "snippet": held.snippet(),
    })
}

/// The mailbox's draft, as an export carries it.
struct WrittenDraft {
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    body: String,
}

/// The draft as a Markdown section, marked a draft.
fn draft_markdown(draft: &WrittenDraft) -> String {
    let mut out = String::new();
    let subject = if draft.subject.trim().is_empty() {
        "(no subject)"
    } else {
        draft.subject.trim()
    };
    let _ = writeln!(out, "## {subject} (draft)\n");
    let _ = writeln!(out, "- Folder: Drafts");
    if !draft.to.is_empty() {
        let _ = writeln!(out, "- To: {}", draft.to);
    }
    if !draft.cc.is_empty() {
        let _ = writeln!(out, "- Cc: {}", draft.cc);
    }
    if !draft.bcc.is_empty() {
        let _ = writeln!(out, "- Bcc: {}", draft.bcc);
    }
    out.push('\n');
    out.push_str(draft.body.trim_end());
    out.push('\n');
    out
}

/// Messages as one Markdown file: a section per message.
fn markdown(messages: &[(&Id, &HeldMessage)]) -> String {
    let mut out = String::new();
    for (index, (_, held)) in messages.iter().enumerate() {
        if index > 0 {
            out.push_str("\n---\n\n");
        }
        let subject = if held.subject.trim().is_empty() {
            "(no subject)"
        } else {
            held.subject.trim()
        };
        let _ = writeln!(out, "## {subject}\n");
        let _ = writeln!(out, "- From: {}", held.from);
        if !held.to.is_empty() {
            let _ = writeln!(out, "- To: {}", held.to);
        }
        if !held.cc.is_empty() {
            let _ = writeln!(out, "- Cc: {}", held.cc);
        }
        let _ = writeln!(out, "- Date: {}\n", held.date());
        out.push_str(held.body.trim_end());
        out.push('\n');
    }
    out
}

/// One message as an RFC 5322 file.
fn eml(held: &HeldMessage) -> Result<Vec<u8>, String> {
    fn addresses(list: &str) -> Vec<mail_builder::headers::address::Address<'static>> {
        crate::transport::parse_addr_list(list)
            .into_iter()
            .map(|addr| mail_builder::headers::address::Address::new_address(addr.name, addr.email))
            .collect()
    }
    let sender = held.sender();
    let mut builder = mail_builder::MessageBuilder::new()
        .from(mail_builder::headers::address::Address::new_address(
            sender.name,
            sender.email,
        ))
        .subject(held.subject.clone())
        .message_id(held.message_id.clone())
        .date(held.minutes.saturating_mul(60))
        .text_body(held.body.clone());
    let to = addresses(&held.to);
    if !to.is_empty() {
        builder = builder.to(mail_builder::headers::address::Address::new_list(to));
    }
    let cc = addresses(&held.cc);
    if !cc.is_empty() {
        builder = builder.cc(mail_builder::headers::address::Address::new_list(cc));
    }
    if let Some(answered) = &held.in_reply_to {
        builder = builder.in_reply_to(answered.clone());
    }
    builder
        .write_to_vec()
        .map_err(|error| format!("the message does not encode: {error}"))
}

/// What a held-mail verb made, answered to its caller: the message a
/// delivery kept, by id and item, and the draft as drafting left it.
pub(crate) fn answer(
    doc: &tilo_resource::Doc,
    action: &str,
    changes: &[automerge::Patch],
) -> Option<serde_json::Value> {
    if action == "draft-message" || action == "draft-reply" {
        let mail = crate::resource::read_mail(doc)?;
        return Some(serde_json::json!({ "draft": mail.draft_summary() }));
    }
    if action != "deliver-message" {
        return None;
    }
    let mail = crate::resource::read_mail(doc)?;
    changes.iter().find_map(|patch| {
        let automerge::PatchAction::PutMap { key, .. } = &patch.action else {
            return None;
        };
        let (id, held) = mail.held.iter().find(|(id, _)| id.as_str() == key)?;
        Some(serde_json::json!({
            "id": id.as_str(),
            "item": format!("mail.message:{}", id.as_str()),
            "message_id": held.message_id,
            "conversation": held.conversation,
        }))
    })
}

/// The answer to `effect` when it reads held mail (`search-messages`,
/// `export-content`), from `mail` alone; `None` for any other effect.
pub(crate) fn effect(
    effect: &crate::actions::MailEffect,
    mail: &Mail,
) -> Option<ui::framework::EffectAnswer> {
    use crate::actions::MailEffect;
    let answer = match effect {
        MailEffect::SearchMessages { query, folder } => mail.search_held(query, *folder),
        MailEffect::ExportContent {
            format,
            message,
            folder,
        } => mail.export_held(format.as_deref(), message.as_deref(), *folder),
        _ => return None,
    };
    Some(answer.map(ui::framework::Effected::value))
}

#[cfg(test)]
mod tests;
