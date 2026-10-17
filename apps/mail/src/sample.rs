//! A representative sample mailbox used when the app runs headless
//! (`--shots`) and by tests. Three messages (one with a PDF attachment, one
//! with an HTML part) are authored as raw RFC 5322 `.eml` blobs and hydrated
//! through [`crate::transport::parse_eml`]; the rest are constructed directly
//! for breadth across folders, threads, read and unread, and starred state.
//!
//! Times are relative to the clock (`ui::clock`), which a capture pins, so a
//! shot shows the same mailbox on every run.

use crate::account::{Account, AccountStore};
use crate::model::{AccountConfig, Addr, Attachment, Folder, Mailbox, Message};
use crate::transport::parse_eml;

const OWNER_NAME: &str = "Sam Rivera";
const OWNER_EMAIL: &str = "sam@rivera.example";

/// What a screenshot shows in place of the person's mail: a mailbox, the
/// accounts beside it and the clock it is read at. Never the person's own.
#[derive(Clone)]
pub struct MailFixture {
    pub mailbox: Mailbox,
    pub accounts: AccountStore,
    pub now_min: i64,
}

impl MailFixture {
    /// The bundled sample mailbox and two sample accounts.
    pub fn bundled() -> Self {
        Self {
            mailbox: sample_mailbox(),
            accounts: sample_account_store(),
            now_min: SAMPLE_NOW_MIN,
        }
    }

    /// Nobody has added an account yet.
    pub fn without_accounts() -> Self {
        Self {
            mailbox: Mailbox::new(AccountConfig::default()),
            accounts: AccountStore::new(),
            now_min: SAMPLE_NOW_MIN,
        }
    }

    /// A configured account before any message has arrived.
    pub fn before_first_fetch() -> Self {
        let accounts = sample_account_store();
        Self {
            mailbox: Mailbox::new(accounts.active_config()),
            accounts,
            now_min: SAMPLE_NOW_MIN,
        }
    }
}

/// Deterministic "now" for the sample corpus so relative timestamps render
/// identically every run. 2026-06-30 14:05 UTC.
pub const SAMPLE_NOW_MIN: i64 = {
    // Equals util::minutes_of(2026, 6, 30, 14, 5); pinned by a test so the
    // literal can never silently drift from the civil-date math.
    29_713_805
};

/// The clock of the hermetic sample corpus.
pub fn sample_now_min() -> i64 {
    SAMPLE_NOW_MIN
}

/// A metadata-only (projected, not yet downloaded) sample attachment. Its
/// bytes aren't present, so the reader shows it but a save requires
/// hydration, as a live IMAP fetch lists an attachment before fetching it.
fn att(filename: &str, mime: &str, size: u64) -> Attachment {
    Attachment::projected(filename, mime, size)
}

struct SampleMessage<'a> {
    from: Addr,
    to: Vec<Addr>,
    subject: &'a str,
    body: &'a str,
    ago_min: i64,
    read: bool,
    starred: bool,
    folder: Folder,
    attachments: Vec<Attachment>,
}

impl SampleMessage<'_> {
    fn build(self, now: i64) -> Message {
        Message {
            id: crate::transport::stable_message_id(&format!(
                "{}/{}/{}",
                self.folder.as_key(),
                self.ago_min,
                self.subject
            )),
            rfc_message_id: None,
            in_reply_to: None,
            from: self.from,
            to: self.to,
            cc: vec![],
            subject: self.subject.into(),
            body: self.body.into(),
            html_body: None,
            attachments: self.attachments,
            timestamp_min: now - self.ago_min,
            read: self.read,
            starred: self.starred,
            folder: self.folder,
            labels: Vec::new(),
            account: String::new(),
            server: None,
        }
    }
}

/// One hour, in minutes: the unit the sample timestamps are expressed in.
const HOUR_MIN: i64 = 60;
/// One day, in minutes.
const DAY_MIN: i64 = 24 * HOUR_MIN;

/// The people the sample mail is from and to.
struct People {
    me: Addr,
    maya: Addr,
    daniel: Addr,
    priya: Addr,
    jonas: Addr,
}

impl People {
    fn new() -> Self {
        Self {
            me: Addr::new(OWNER_NAME, OWNER_EMAIL),
            maya: Addr::new("Maya Chen", "maya@northwind.example"),
            daniel: Addr::new("Daniel Okoro", "daniel@northwind.example"),
            priya: Addr::new("Priya Nair", "priya@northwind.example"),
            jonas: Addr::new("Jonas Weber", "jonas@weber.example"),
        }
    }
}

/// The owner's account settings, so the account sheet has real values.
fn owner_config() -> AccountConfig {
    AccountConfig {
        display_name: OWNER_NAME.into(),
        email: OWNER_EMAIL.into(),
        imap_host: "imap.rivera.example".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.rivera.example".into(),
        smtp_port: 465,
        smtp_tls: true,
        username: OWNER_EMAIL.into(),
        auth: crate::model::AuthMethod::Password,
        import_pending: false,
        password: "correct-horse-battery".into(),
    }
}

/// Build the bundled mailbox, its times relative to the clock.
pub fn sample_mailbox() -> Mailbox {
    sample_mailbox_at(sample_now_min())
}

/// Build the bundled mailbox with `now` (minutes since the epoch) as the
/// present.
pub fn sample_mailbox_at(now: i64) -> Mailbox {
    let mut mb = Mailbox::new(owner_config());
    let people = People::new();
    insert_launch_plan_thread(&mut mb, &people, now);
    insert_standalone_inbox_items(&mut mb, &people, now);
    insert_sent_archive_and_junk(&mut mb, &people, now);

    // Three messages hydrated through the real mail-parser path (plain text,
    // a PDF attachment, and an HTML newsletter), so the offline mailbox runs
    // the same parse code a live IMAP fetch does.
    if let Some(mut m) = parse_eml(RAW_PLAIN.as_bytes(), now - 30, Folder::Inbox) {
        m.read = false;
        m.starred = false;
        mb.insert(m);
    }
    if let Some(mut m) = parse_eml(RAW_WITH_PDF.as_bytes(), now - 7 * HOUR_MIN, Folder::Inbox) {
        m.read = true;
        mb.insert(m);
    }
    // The newsletter carries a remote tracking image the reader blocks.
    if let Some(mut m) = parse_eml(RAW_HTML.as_bytes(), now - 52, Folder::Inbox) {
        m.read = false;
        m.starred = true;
        mb.insert(m);
    }
    mb
}

/// The one conversation of several messages, so the list's threading and its
/// unread newest message have something to group.
fn insert_launch_plan_thread(mb: &mut Mailbox, people: &People, now: i64) {
    let root_id = "sample-launch-plan-root@northwind.example";
    let mut root = SampleMessage {
        from: people.maya.clone(),
        to: vec![people.me.clone(), people.daniel.clone()],
        subject: "Spring launch plan",
        body: "Hi both,\n\nHere is a first pass at the order for the spring launch: the \
email campaign first, then the website, then the press kit. Does that sequence work for \
you?\n\nMaya",
        ago_min: 2 * HOUR_MIN + 23,
        read: true,
        starred: true,
        folder: Folder::Inbox,
        attachments: vec![],
    }
    .build(now);
    root.rfc_message_id = Some(root_id.into());
    mb.insert(root);
    let mut reply = SampleMessage {
        from: people.daniel.clone(),
        to: vec![people.me.clone(), people.maya.clone()],
        subject: "Re: Spring launch plan",
        body: "I would put the website first. The campaign links to it, so it has to be \
live before anything goes out.\n\nDaniel",
        ago_min: HOUR_MIN + 47,
        read: true,
        starred: false,
        folder: Folder::Inbox,
        attachments: vec![],
    }
    .build(now);
    reply.rfc_message_id = Some("sample-launch-plan-reply@northwind.example".into());
    reply.in_reply_to = Some(root_id.into());
    mb.insert(reply);
    let mut latest = SampleMessage {
        from: people.maya.clone(),
        to: vec![people.me.clone(), people.daniel.clone()],
        subject: "Re: Spring launch plan",
        body: "Agreed. New order: website, press kit, then the campaign. I attached the \
revised timeline. Sam, could you review the press section before Friday?",
        ago_min: 38,
        read: false,
        starred: false,
        folder: Folder::Inbox,
        attachments: vec![att("launch-timeline.pdf", "application/pdf", 184_320)],
    }
    .build(now);
    latest.rfc_message_id = Some("sample-launch-plan-latest@northwind.example".into());
    latest.in_reply_to = Some(root_id.into());
    mb.insert(latest);
}

/// The messages that stand alone: a quick question, a budget, an invitation,
/// a neighbourhood list and a delivery notice.
fn insert_standalone_inbox_items(mb: &mut Mailbox, people: &People, now: i64) {
    mb.insert(
        SampleMessage {
            from: people.jonas.clone(),
            to: vec![people.me.clone()],
            subject: "Coffee before the design review?",
            body: "Do you have time for a coffee at 9:15 before the design review? I have a \
few thoughts on the new onboarding screens.",
            ago_min: 3,
            read: false,
            starred: false,
            folder: Folder::Inbox,
            attachments: vec![],
        }
        .build(now),
    );
    mb.insert(
        SampleMessage {
            from: people.priya.clone(),
            to: vec![people.me.clone()],
            subject: "Budget numbers for the second quarter",
            body: "I went through the second-quarter numbers line by line and they match the \
forecast. Travel is tight; should we move the offsite to September?",
            ago_min: 5 * HOUR_MIN,
            read: true,
            starred: true,
            folder: Folder::Inbox,
            attachments: vec![att("q2-budget.csv", "text/csv", 9_204)],
        }
        .build(now),
    );
    mb.insert(
        SampleMessage {
            from: Addr::new("Northwind Calendar", "calendar@northwind.example"),
            to: vec![people.me.clone()],
            subject: "Invitation: Design review on Thursday at 11:00",
            body: "Maya Chen invited you to the design review.\nWhen: Thursday, 11:00 to \
11:45\nWhere: Room 4B and online",
            ago_min: 9 * HOUR_MIN,
            read: false,
            starred: false,
            folder: Folder::Inbox,
            attachments: vec![att("invite.ics", "text/calendar", 1_842)],
        }
        .build(now),
    );
    mb.insert(
        SampleMessage {
            from: Addr::new("Tom Becker", "tom@neighbours.example"),
            to: vec![Addr::bare("garden@lists.neighbours.example")],
            subject: "Community garden: watering rota for July",
            body: "Thanks to everyone who signed up. The rota for July is on the shed door \
and in the shared folder. Please swap directly with each other if you are away.",
            ago_min: DAY_MIN + 2 * HOUR_MIN,
            read: true,
            starred: false,
            folder: Folder::Inbox,
            attachments: vec![],
        }
        .build(now),
    );
    mb.insert(
        SampleMessage {
            from: Addr::new("Linden Books", "orders@lindenbooks.example"),
            to: vec![people.me.clone()],
            subject: "Your order has shipped",
            body: "Good news: your order of two books is on its way and should arrive on \
Thursday. You can follow the delivery from your account page.",
            ago_min: 2 * DAY_MIN + 5 * HOUR_MIN,
            read: true,
            starred: false,
            folder: Folder::Inbox,
            attachments: vec![],
        }
        .build(now),
    );
}

/// The Sent, Archive and Junk folders of the bundled corpus.
fn insert_sent_archive_and_junk(mb: &mut Mailbox, people: &People, now: i64) {
    mb.insert(
        SampleMessage {
            from: people.me.clone(),
            to: vec![people.maya.clone(), people.daniel.clone()],
            subject: "Re: Spring launch plan",
            body: "Thanks both. I will take the press kit and have a draft ready by \
Wednesday.",
            ago_min: 50,
            read: true,
            starred: false,
            folder: Folder::Sent,
            attachments: vec![],
        }
        .build(now),
    );
    mb.insert(
        SampleMessage {
            from: people.me.clone(),
            to: vec![people.priya.clone()],
            subject: "Re: Budget numbers for the second quarter",
            body: "September works for me. I will ask the venue whether they can hold the \
same rooms.",
            ago_min: 4 * HOUR_MIN + 30,
            read: true,
            starred: false,
            folder: Folder::Sent,
            attachments: vec![],
        }
        .build(now),
    );
    mb.insert(
        SampleMessage {
            from: people.daniel.clone(),
            to: vec![people.me.clone()],
            subject: "Welcome to the team",
            body: "Welcome aboard! Here are the team handbook and the review schedule. Ask \
me anything in your first weeks.",
            ago_min: 9 * DAY_MIN,
            read: true,
            starred: false,
            folder: Folder::Archive,
            attachments: vec![att("team-handbook.pdf", "application/pdf", 512_000)],
        }
        .build(now),
    );
    mb.insert(
        SampleMessage {
            from: Addr::new("Prize Centre", "winner@prize.example"),
            to: vec![people.me.clone()],
            subject: "Congratulations, you have won a free monitor",
            body: "Dear winner, click here within 24 hours to claim your prize.",
            ago_min: 7 * HOUR_MIN,
            read: false,
            starred: false,
            folder: Folder::Junk,
            attachments: vec![],
        }
        .build(now),
    );
}

/// A bundled store of two accounts so the sidebar shows more than one
/// account offline. The first owns the sample [`Mailbox`]; the second is a
/// work account.
pub fn sample_account_store() -> AccountStore {
    let work = AccountConfig {
        display_name: OWNER_NAME.into(),
        email: "sam@northwind.example".into(),
        imap_host: "imap.northwind.example".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.northwind.example".into(),
        smtp_port: 587,
        smtp_tls: true,
        username: "sam@northwind.example".into(),
        auth: crate::model::AuthMethod::Password,
        import_pending: false,
        password: "correct-horse-battery".into(),
    };
    let mut personal = Account::new(owner_config());
    personal.tint = 0;
    let mut work = Account::new(work);
    work.tint = 3;
    work.collapsed = true;
    AccountStore {
        accounts: vec![personal, work],
        active: 0,
    }
}

/// A plain text/plain message, parsed live by mail-parser.
const RAW_PLAIN: &str = "From: Lena Fischer <lena@fischer.example>\r\n\
To: sam@rivera.example\r\n\
Subject: Photos from the weekend\r\n\
Message-ID: <weekend-2026@fischer.example>\r\n\
Date: Tue, 30 Jun 2026 08:30:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
The photos from the lake are in the shared album now. The one of the boat at \
sunset came out really well. Shall we print a few for your parents?\r\n";

/// A multipart/mixed message with a PDF attachment, parsed live.
const RAW_WITH_PDF: &str = "From: Lumen Energy <billing@lumen-energy.example>\r\n\
To: sam@rivera.example\r\n\
Subject: Your June invoice\r\n\
Message-ID: <invoice-2026-06@lumen-energy.example>\r\n\
Date: Tue, 30 Jun 2026 07:05:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"=_sample_boundary_=\"\r\n\
\r\n\
--=_sample_boundary_=\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Your invoice for June is ready. The amount will be collected on 15 July. \
The invoice is attached.\r\n\
--=_sample_boundary_=\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"invoice-june.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQgZmFrZSBwZGYgYm9keSBmb3IgdGhlIHNhbXBsZSBtYWlsYm94Lg==\r\n\
--=_sample_boundary_=--\r\n";

/// A multipart/alternative message with a plain-text part and a rich HTML
/// part (including a **remote** image the reader must block by default),
/// parsed live.
const RAW_HTML: &str = "From: Field Notes <hello@fieldnotes.example>\r\n\
To: sam@rivera.example\r\n\
Subject: Field Notes 27: gardens on the roof\r\n\
Message-ID: <fieldnotes-27@fieldnotes.example>\r\n\
Date: Tue, 30 Jun 2026 09:50:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"=_sample_alt_=\"\r\n\
\r\n\
--=_sample_alt_=\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
This issue: three city roofs that turned into gardens, and what they grow. \
Read on.\r\n\
--=_sample_alt_=\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body style=\"font-family:sans-serif\">\
<h1>Field Notes</h1>\
<img src=\"https://track.fieldnotes.example/open.gif?u=sam\" width=\"1\" height=\"1\">\
<p>This issue: <strong>three city roofs</strong> that turned into gardens, \
and what they grow.</p>\
<ul><li>Tomatoes in crates</li><li>Bees above the bakery</li>\
<li>A rain garden on a car park</li></ul>\
<blockquote>\u{201c}It changed how the whole street talks to each other.\u{201d}</blockquote>\
<p><a href=\"https://fieldnotes.example/27\">Read the full issue</a></p>\
</body></html>\r\n\
--=_sample_alt_=--\r\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_corpus_is_populated_threaded_and_hydrated() {
        let mb = sample_mailbox();
        // Populated across folders.
        assert!(
            mb.count(Folder::Inbox) >= 8,
            "inbox: {}",
            mb.count(Folder::Inbox)
        );
        assert!(mb.count(Folder::Sent) >= 2);
        assert!(mb.count(Folder::Archive) >= 1);
        assert!(mb.count(Folder::Junk) >= 1);
        assert!(mb.unread_count(Folder::Inbox) > 0);

        // The invoice was parsed from raw MIME and carries its PDF with bytes.
        let invoice = mb
            .messages
            .values()
            .find(|m| m.from.email == "billing@lumen-energy.example")
            .expect("parsed invoice present");
        assert_eq!(invoice.attachments.len(), 1);
        assert_eq!(invoice.attachments[0].filename, "invoice-june.pdf");
        assert!(invoice.attachments[0].is_downloadable());
        assert!(!invoice.attachments[0].content.is_empty());
        // The plain message was parsed too.
        assert!(
            mb.messages
                .values()
                .any(|m| m.from.email == "lena@fischer.example")
        );
        // A hand-built attachment is projected: metadata, no bytes yet.
        let timeline = mb
            .messages
            .values()
            .flat_map(|m| m.attachments.iter())
            .find(|a| a.filename == "launch-timeline.pdf")
            .expect("projected attachment");
        assert!(!timeline.is_downloadable());

        // The launch thread's three messages collapse into one row.
        let threads = mb.threads_in(Folder::Inbox);
        let launch = threads
            .iter()
            .find(|t| t.subject == "Spring launch plan")
            .expect("launch thread present");
        assert_eq!(launch.message_ids.len(), 3);

        // Every message is in the past.
        let now = sample_now_min();
        assert!(mb.messages.values().all(|m| m.timestamp_min <= now));
    }
}
