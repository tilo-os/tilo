//! What Mail's search finds: the folders, and every message already loaded
//! in memory (excluding Trash) by its subject, sender or body.
//!
//! Folders are the mailbox resource's own search entries; loaded messages
//! are the mail surface's, because their subjects, senders and bodies come
//! from the engine rather than the document. Both are indexed into the one
//! search source the framework keeps for the app, so a query never reaches
//! the IMAP engine: the live server search stays in Mail's own search field.

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

/// The start of `body`, at most [`BODY_SEARCHED`] bytes, cut on a character.
fn searched_part(body: &str) -> &str {
    if body.len() <= BODY_SEARCHED {
        return body;
    }
    let mut end = BODY_SEARCHED;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    body.get(..end).unwrap_or_default()
}

fn message_entry(message: &Message) -> SearchEntry {
    let title = if message.subject.trim().is_empty() {
        NO_SUBJECT.to_owned()
    } else {
        message.subject.clone()
    };
    // Ranked as the mailbox ranks them: the subject first, then who sent
    // it, the address typed verbatim, the labels, and the body last.
    SearchEntry::new(title, searched_part(&message.body).to_owned())
        .field(message.from.email.clone(), SearchWeight::Identity)
        .field(message.labels.join(" "), SearchWeight::Metadata)
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
                    // A match inside a word takes four letters.
                    .search("ÁLAG")
                    .iter()
                    .any(|hit| hit.title == message.subject),
                "Unicode case folding keeps a mixed-case substring discoverable"
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
