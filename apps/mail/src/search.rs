//! What Mail's search finds: the folders, and every message already loaded
//! in memory (excluding Trash) by its subject, sender, recipients or body.
//!
//! Folders are the mailbox resource's own search entries; loaded messages
//! are the window's, published by its view, because their subjects,
//! senders and bodies come from the engine rather than the document. Both
//! are indexed into the one search source the framework keeps for the app,
//! so a query never reaches the IMAP engine: the live server search stays in
//! Mail's own search field.

use ui::tilo_resource::{SearchEntry, SearchWeight};

use crate::model::{Folder, Mailbox, Message};

/// The result group messages are listed under.
pub const MAIL_KIND: &str = "Mail";
/// The result group folders are listed under.
pub const FOLDER_KIND: &str = "Mail folders";

/// Shown for a message whose subject is blank.
const NO_SUBJECT: &str = "(no subject)";

/// The lucide name of a folder's icon ([`Folder::icon`]).
fn folder_icon(folder: Folder) -> &'static str {
    match folder {
        Folder::Inbox => "inbox",
        Folder::Starred => "star",
        Folder::Sent => "send",
        Folder::Drafts => "pencil",
        Folder::Archive => "archive",
        Folder::Trash => "trash",
        Folder::Junk => "ban",
    }
}

/// Every system folder, and the custom folders `custom` names (id, name).
pub fn folder_entries(custom: &[(String, String)]) -> Vec<SearchEntry> {
    let system = Folder::ALL.into_iter().map(|folder| {
        SearchEntry::new(folder.title(), "")
            .anchored(crate::resource::folder_resource(folder.as_key()))
            .grouped(FOLDER_KIND)
            .subtitled("Mail folder")
            .with_icon(folder_icon(folder))
    });
    let custom = custom.iter().map(|(id, name)| {
        SearchEntry::new(name.clone(), "")
            .anchored(crate::resource::folder_resource(id))
            .grouped(FOLDER_KIND)
            .subtitled("Mail folder")
            .with_icon("folder")
    });
    system.chain(custom).collect()
}

/// Every message of `mailbox` outside Trash.
pub fn message_entries(mailbox: &Mailbox) -> Vec<SearchEntry> {
    mailbox
        .messages
        .values()
        .filter(|message| message.folder != Folder::Trash)
        .map(message_entry)
        .collect()
}

/// How much of a body search reads: the part a person remembers a mail
/// by. The search source is rebuilt from every kept message whenever
/// someone searches, so a mailbox of thousands of long mails must not hand
/// it whole bodies.
const BODY_SEARCHED: usize = 4 * 1024;

/// How much of a message's recipients search reads: enough for every name
/// and address of an ordinary message, bounded for a mailing to hundreds.
const RECIPIENTS_SEARCHED: usize = 1024;

/// The start of `body`, at most [`BODY_SEARCHED`] bytes, cut on a character.
fn searched_part(body: &str) -> &str {
    cut(body, BODY_SEARCHED)
}

/// The start of `text`, at most `most` bytes, cut on a character.
fn cut(text: &str, most: usize) -> &str {
    if text.len() <= most {
        return text;
    }
    let mut end = most;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

/// Who a message went to, by name and address: what finds a sent message
/// by the person it was sent to.
fn recipients(message: &Message) -> String {
    let mut out = String::new();
    for addr in message.to.iter().chain(&message.cc) {
        if out.len() >= RECIPIENTS_SEARCHED {
            break;
        }
        if let Some(name) = addr.name.as_deref().filter(|name| !name.is_empty()) {
            out.push_str(name);
            out.push(' ');
        }
        out.push_str(&addr.email);
        out.push(' ');
    }
    let kept = cut(&out, RECIPIENTS_SEARCHED).trim_end().len();
    out.truncate(kept);
    out
}

fn message_entry(message: &Message) -> SearchEntry {
    let title = if message.subject.trim().is_empty() {
        NO_SUBJECT.to_owned()
    } else {
        message.subject.clone()
    };
    // Ranked as the mailbox ranks them: the subject first, then who sent
    // it, the address typed verbatim, the labels and recipients, and the
    // body last.
    SearchEntry::new(title, searched_part(&message.body).to_owned())
        .field(message.from.email.clone(), SearchWeight::Identity)
        .field(message.labels.join(" "), SearchWeight::Metadata)
        .field(recipients(message), SearchWeight::Metadata)
        .anchored(format!("mail.message:{}", message.id))
        .grouped(MAIL_KIND)
        .subtitled(message.from.display())
        .with_icon("mail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Mail;
    use ui::search::SearchProvider;

    fn sample() -> Mailbox {
        crate::sample::sample_mailbox()
    }

    fn search(mailbox: &Mailbox) -> impl SearchProvider + use<> {
        let mut entries = folder_entries(&[]);
        entries.extend(message_entries(mailbox));
        ui::framework::testing::search_source::<Mail>(entries)
    }

    #[test]
    fn search_matches_subject_sender_and_folded_infixes() {
        {
            let mb = sample();
            let some = mb
                .messages
                .values()
                .find(|message| message.folder != Folder::Trash)
                .expect("sample mailbox is populated")
                .clone();
            let word = some
                .subject
                .split_whitespace()
                .next()
                .unwrap_or(&some.subject);
            let hits: Vec<_> = search(&mb)
                .search(word)
                .into_iter()
                .filter(|hit| hit.kind == MAIL_KIND)
                .collect();
            assert!(
                !hits.is_empty(),
                "a real subject word surfaces at least its message"
            );
            assert!(
                hits.iter().all(|r| r.subtitle.is_some()),
                "every row shows a sender"
            );
        }
        {
            let mut mailbox = sample();
            let mut message = mailbox
                .messages
                .values()
                .next()
                .expect("seeded message")
                .clone();
            message.id = uuid::Uuid::new_v4();
            message.folder = Folder::Inbox;
            message.subject = "Málaga release notes".into();
            message.body.clear();
            message.attachments.clear();
            mailbox.insert(message.clone());
            assert!(
                search(&mailbox)
                    .search("ÁLAG")
                    .iter()
                    .any(|hit| hit.title == message.subject),
                "Unicode case folding keeps a mixed-case substring discoverable"
            );
        }
    }

    /// A sent message is found by the person it went to, by name or
    /// address, though neither is in its subject, sender or body.
    #[test]
    fn a_sent_message_is_found_by_its_recipient() {
        let mut mailbox = sample();
        let mut sent = mailbox
            .messages
            .values()
            .next()
            .expect("seeded message")
            .clone();
        sent.id = uuid::Uuid::new_v4();
        sent.folder = Folder::Sent;
        sent.subject = "Launch plan".into();
        sent.body = "The plan is attached.".into();
        sent.to = vec![crate::model::Addr::new(
            "Grace Hopper",
            "grace@navy.example",
        )];
        sent.cc = vec![crate::model::Addr::bare("ops@navy.example")];
        mailbox.insert(sent);
        for query in ["Grace Hopper", "grace@navy.example", "ops@navy"] {
            assert!(
                search(&mailbox)
                    .search(query)
                    .iter()
                    .any(|hit| hit.title == "Launch plan"),
                "`{query}` finds the message sent to them"
            );
        }
    }

    #[test]
    fn trash_stays_out_and_every_hit_opens_its_resource() {
        let mb = sample();
        let entries = message_entries(&mb);
        assert!(
            entries.iter().all(|entry| {
                let id = entry
                    .anchor
                    .as_deref()
                    .and_then(|anchor| anchor.strip_prefix("mail.message:"))
                    .and_then(|id| id.parse::<uuid::Uuid>().ok())
                    .expect("a message anchor");
                mb.messages
                    .values()
                    .any(|message| message.id == id && message.folder != Folder::Trash)
            }),
            "trashed mail is not offered"
        );
        let folders = search(&mb).search("archive");
        let archive = folders
            .iter()
            .find(|hit| hit.kind == FOLDER_KIND)
            .expect("the archive folder is found");
        assert_eq!(archive.subtitle.as_deref(), Some("Mail folder"));
        assert_eq!(archive.icon, Some(ui::icons::Icon::Archive));
        assert_eq!(
            archive.anchor(),
            crate::resource::folder_resource(Folder::Archive.as_key())
        );
    }

    #[test]
    fn a_late_message_stays_reachable_in_a_large_mailbox() {
        let mut mailbox = sample();
        let template = mailbox
            .messages
            .values()
            .next()
            .expect("seeded message")
            .clone();
        for n in 0..3_000 {
            let mut message = template.clone();
            message.id = uuid::Uuid::new_v4();
            message.folder = Folder::Inbox;
            message.subject = format!("ordinary record {n}");
            message.body.clear();
            mailbox.insert(message);
        }
        let source = search(&mailbox);
        let first = source.search_page(&ui::search::SearchQuery::new("ordinary"), None, 7, 3);
        assert!(first.next.is_some(), "a broad query continues");
        assert!(first.results.len() <= 3);
        assert!(
            source
                .search("record 2999")
                .iter()
                .any(|hit| hit.title == "ordinary record 2999")
        );
    }
}

/// What an agent's `search-messages` asks for: every word of `query`
/// anywhere, words of the sender and the subject, a folder, a span of dates
/// and whether there are attachments.
#[derive(Clone, Debug, Default)]
pub(crate) struct MessageQuery {
    words: Vec<String>,
    from: Vec<String>,
    subject: Vec<String>,
    folder: Option<Folder>,
    since_min: Option<i64>,
    until_min: Option<i64>,
    has_attachment: Option<bool>,
}

/// The most messages one search answers, newest first.
const MOST_FOUND: usize = 50;

fn words_of(text: Option<&str>) -> Vec<String> {
    text.unwrap_or("")
        .split_whitespace()
        .map(str::to_lowercase)
        .collect()
}

/// `date` (RFC 3339, a day or a time) as minutes since the Unix epoch.
fn minutes_at(date: &str) -> Result<i64, String> {
    let date = date.trim();
    let parsed = mail_parser::DateTime::parse_rfc3339(date)
        .or_else(|| mail_parser::DateTime::parse_rfc3339(&format!("{date}T00:00:00Z")))
        .or_else(|| mail_parser::DateTime::parse_rfc822(date))
        .ok_or_else(|| format!("`{date}` is not a date: give RFC 3339, as 2026-10-29"))?;
    Ok(parsed.to_timestamp().div_euclid(60))
}

impl MessageQuery {
    /// The query `effect` asks, or why it asks none.
    pub(crate) fn of(effect: &crate::actions::MailEffect) -> Result<Self, String> {
        let crate::actions::MailEffect::SearchMessages {
            query,
            folder,
            from,
            subject,
            since,
            until,
            has_attachment,
        } = effect
        else {
            return Err("not a search".into());
        };
        let query = Self {
            words: words_of(query.as_deref()),
            from: words_of(from.as_deref()),
            subject: words_of(subject.as_deref()),
            folder: *folder,
            since_min: since.as_deref().map(minutes_at).transpose()?,
            until_min: until.as_deref().map(minutes_at).transpose()?,
            has_attachment: *has_attachment,
        };
        if query.words.is_empty()
            && query.from.is_empty()
            && query.subject.is_empty()
            && query.folder.is_none()
            && query.since_min.is_none()
            && query.until_min.is_none()
            && query.has_attachment.is_none()
        {
            return Err(
                "give words, a sender, a subject, a folder, dates or attachments to search by"
                    .into(),
            );
        }
        Ok(query)
    }

    /// Whether `message` is one this query finds.
    pub(crate) fn finds(&self, message: &Message) -> bool {
        let holds = |text: &str, words: &[String]| {
            let text = text.to_lowercase();
            words.iter().all(|word| text.contains(word.as_str()))
        };
        let sender = format!(
            "{} {}",
            message.from.name.as_deref().unwrap_or(""),
            message.from.email
        );
        let everything = format!(
            "{sender}\n{}\n{}\n{}\n{}",
            message
                .to
                .iter()
                .chain(&message.cc)
                .map(crate::model::Addr::rfc5322)
                .collect::<Vec<_>>()
                .join(", "),
            message.subject,
            message.body,
            message
                .attachments
                .iter()
                .map(|attachment| attachment.filename.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        self.folder.is_none_or(|folder| message.folder == folder)
            && self
                .since_min
                .is_none_or(|since| message.timestamp_min >= since)
            && self
                .until_min
                .is_none_or(|until| message.timestamp_min < until)
            && self
                .has_attachment
                .is_none_or(|wanted| message.has_attachments() == wanted)
            && holds(&sender, &self.from)
            && holds(&message.subject, &self.subject)
            && holds(&everything, &self.words)
    }

    /// The messages of `messages` this query finds, newest first, at most
    /// [`MOST_FOUND`], as a search answers them.
    pub(crate) fn answer<'a>(
        &self,
        messages: impl Iterator<Item = &'a Message>,
    ) -> serde_json::Value {
        let mut found: Vec<&Message> = messages.filter(|message| self.finds(message)).collect();
        let total = found.len();
        found.sort_by(|a, b| {
            b.timestamp_min
                .cmp(&a.timestamp_min)
                .then_with(|| a.id.cmp(&b.id))
        });
        serde_json::json!({
            "found": total,
            "messages": found.iter().take(MOST_FOUND).map(|message| listed(message)).collect::<Vec<_>>(),
        })
    }
}

/// One message as a search lists it.
pub(crate) fn listed(message: &Message) -> serde_json::Value {
    serde_json::json!({
        "id": message.id.to_string(),
        "item": format!("mail.message:{}", message.id),
        "label": crate::actions::message_label_of(message),
        "from": message.from.rfc5322(),
        "to": message.to.iter().map(crate::model::Addr::rfc5322).collect::<Vec<_>>(),
        "subject": message.subject,
        "date": mail_parser::DateTime::from_timestamp(message.timestamp_min.saturating_mul(60)).to_rfc3339(),
        "folder": message.folder.as_key(),
        "read": message.read,
        "account": message.account,
        "snippet": message.snippet(),
        "attachments": message.shown_attachments().map(|(_, attachment)| attachment.filename.clone()).collect::<Vec<_>>(),
    })
}
