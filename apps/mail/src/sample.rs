//! A representative sample mailbox used when the app runs headless
//! (`--shots`) or before a live account is configured. Two messages (including
//! one with a PDF attachment) are authored as raw RFC 5322 `.eml` blobs and
//! hydrated through [`crate::transport::parse_eml`]. The rest are constructed
//! directly for breadth across
//! folders, threads, read/unread, and starred state.

use uuid::Uuid;

use crate::account::{Account, AccountStore};
use crate::model::{AccountConfig, Addr, Attachment, Folder, Mailbox, Message};
use crate::transport::parse_eml;

/// Deterministic "now" for the sample corpus so relative timestamps render
/// identically every run. 2026-06-30 14:05 UTC.
pub const SAMPLE_NOW_MIN: i64 = {
    // Equals util::minutes_of(2026, 6, 30, 14, 5); pinned by a test so the
    // literal can never silently drift from the civil-date math.
    29_713_805
};

/// A metadata-only (projected, not yet downloaded) sample attachment. Its
/// bytes aren't present, so the reader shows it but a save requires hydration —
/// mirroring how a live IMAP fetch lists an attachment before fetching it.
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
    fn build(self) -> Message {
        Message {
            id: Uuid::new_v4(),
            rfc_message_id: None,
            in_reply_to: None,
            from: self.from,
            to: self.to,
            cc: vec![],
            subject: self.subject.into(),
            body: self.body.into(),
            html_body: None,
            attachments: self.attachments,
            timestamp_min: SAMPLE_NOW_MIN - self.ago_min,
            read: self.read,
            starred: self.starred,
            folder: self.folder,
            labels: Vec::new(),
        }
    }
}

struct InboxCorrespondents<'a> {
    ci: Addr,
    katherine: &'a Addr,
    alan: Addr,
    linus: Addr,
    calendar: Addr,
}

/// One hour, in minutes — the unit the sample timestamps are expressed in.
const HOUR_MIN: i64 = 60;

/// Build the bundled mailbox. The account is a plausible self-host config so
/// the account-setup screen has real values to display.
pub fn sample_mailbox() -> Mailbox {
    let me = Addr::new("You", "you@example.test");
    let account = AccountConfig {
        display_name: "You".into(),
        email: "you@example.test".into(),
        imap_host: "imap.example.test".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.example.test".into(),
        smtp_port: 465,
        smtp_tls: true,
        username: "you@example.test".into(),
        auth: crate::model::AuthMethod::Password,
        password: "********".into(),
    };
    let mut mb = Mailbox::new(account);

    let ada = Addr::new("Ada Lovelace", "ada@analytical.example");
    let grace = Addr::new("Grace Hopper", "grace@navy.example");
    let alan = Addr::new("Alan Turing", "alan@bletchley.example");
    let katherine = Addr::new("Katherine Johnson", "katherine@nasa.example");
    let linus = Addr::new("Linus Torvalds", "linus@kernel.example");
    let ci = Addr::new("Tilo CI", "ci@example.test");
    let calendar = Addr::new("Calendar", "noreply@cal.example.test");

    insert_launch_plan_thread(&mut mb, &me, &ada, &grace);
    insert_standalone_inbox_items(
        &mut mb,
        &me,
        InboxCorrespondents {
            ci,
            katherine: &katherine,
            alan,
            linus,
            calendar,
        },
    );

    insert_sent_archive_and_junk(&mut mb, me, ada, grace, katherine);

    // ---- Two messages hydrated through the REAL mail-parser path ----
    // (one plain, one multipart/mixed with a PDF attachment). This proves
    // the offline screenshot is running the same parse code the live IMAP
    // fetch would.
    if let Some(mut m) = parse_eml(RAW_PLAIN.as_bytes(), SAMPLE_NOW_MIN - 30, Folder::Inbox) {
        m.read = false;
        m.starred = false;
        mb.insert(m);
    }
    if let Some(mut m) = parse_eml(
        RAW_WITH_PDF.as_bytes(),
        SAMPLE_NOW_MIN - 7 * 60,
        Folder::Inbox,
    ) {
        m.read = true;
        mb.insert(m);
    }
    // A rich HTML newsletter (with a remote tracking image) parsed live, so the
    // reader exercises the sandboxed WebView path and the remote-content block.
    if let Some(mut m) = parse_eml(RAW_HTML.as_bytes(), SAMPLE_NOW_MIN - 52, Folder::Inbox) {
        m.read = false;
        m.starred = true;
        mb.insert(m);
    }

    mb
}

/// Thread 1 of the bundled corpus: the only multi-message conversation, so
/// the list's threading and unread-head rendering have something to group.
fn insert_launch_plan_thread(mb: &mut Mailbox, me: &Addr, ada: &Addr, grace: &Addr) {
    // ---- Thread 1: "Launch plan for the spatial shell" (3 msgs, unread head)
    let root_id = "sample-launch-plan-root@example.test";
    let mut root = SampleMessage {
        from: ada.clone(),
        to: vec![me.clone()],
        subject: "Launch plan for the spatial shell",
        body: "Hi team,\n\nI sketched the rollout for the 2D-cluster window model. \
First milestone is the tiling engine, then per-cluster sandboxing, then the \
collaboration buffer streaming. Thoughts on sequencing?\n\n— Ada",
        ago_min: 2 * HOUR_MIN + 23,
        read: true,
        starred: true,
        folder: Folder::Inbox,
        attachments: vec![],
    }
    .build();
    root.rfc_message_id = Some(root_id.into());
    mb.insert(root);
    let mut reply = SampleMessage {
        from: grace.clone(),
        to: vec![me.clone(), ada.clone()],
        subject: "Re: Launch plan for the spatial shell",
        body: "Sequencing looks right. I'd pull sandboxing forward — it's the \
security boundary everything else leans on. Let's not ship streaming before \
the bubblewrap policy is solid.\n\nGrace",
        ago_min: HOUR_MIN + 47,
        read: true,
        starred: false,
        folder: Folder::Inbox,
        attachments: vec![],
    }
    .build();
    reply.rfc_message_id = Some("sample-launch-plan-reply@example.test".into());
    reply.in_reply_to = Some(root_id.into());
    mb.insert(reply);
    let mut latest = SampleMessage {
        from: ada.clone(),
        to: vec![me.clone(), grace.clone()],
        subject: "Re: Launch plan for the spatial shell",
        body: "Agreed. New order: tiling → sandbox → streaming. I attached the \
revised gantt. Can you review the collaboration section before Friday?",
        ago_min: 38,
        read: false,
        starred: false,
        folder: Folder::Inbox,
        attachments: vec![att("rollout-gantt.pdf", "application/pdf", 184_320)],
    }
    .build();
    latest.rfc_message_id = Some("sample-launch-plan-latest@example.test".into());
    latest.in_reply_to = Some(root_id.into());
    mb.insert(latest);
}

/// The one-off inbox items: a CI failure, two short replies, a mailing-list
/// post and a calendar invite. Each stands alone rather than threading.
fn insert_standalone_inbox_items(
    mb: &mut Mailbox,
    me: &Addr,
    correspondents: InboxCorrespondents<'_>,
) {
    // ---- Thread 2: CI failure (unread, attachment: log)
    mb.insert(
        SampleMessage {
            from: correspondents.ci,
            to: vec![me.clone()],
            subject: "Build #4821 failed on shell --lib",
            body: "Pipeline tilo/tilo run #4821 FAILED.\n\nstage: cargo test -p shell --lib\n\
error: process didn't exit successfully: SIGSEGV (parallel wgpu instance \
contention?)\n\nFull log attached. Re-run threshold was 4 OK / 8 SIGSEGV.",
            ago_min: 14,
            read: false,
            starred: false,
            folder: Folder::Inbox,
            attachments: vec![att("build-4821.log", "text/plain", 28_716)],
        }
        .build(),
    );

    // ---- Thread 3: Katherine, calculations (read)
    mb.insert(
        SampleMessage {
            from: correspondents.katherine.clone(),
            to: vec![me.clone()],
            subject: "Re-check the trajectory math",
            body: "I ran the re-entry numbers by hand and they line up with the \
solver. Margins are tight on the third burn — want me to widen the window?",
            ago_min: 5 * HOUR_MIN,
            read: true,
            starred: true,
            folder: Folder::Inbox,
            attachments: vec![att("trajectory.csv", "text/csv", 9_204)],
        }
        .build(),
    );

    // ---- Thread 4: Alan, single short (unread)
    mb.insert(
        SampleMessage {
            from: correspondents.alan,
            to: vec![me.clone()],
            subject: "Coffee before the standup?",
            body: "Fancy a coffee at 9:15 before we dive into the compositor review? \
I have thoughts on the focus model.",
            ago_min: 3,
            read: false,
            starred: false,
            folder: Folder::Inbox,
            attachments: vec![],
        }
        .build(),
    );

    // ---- Thread 5: Linus, mailing-list style (read, long)
    mb.insert(
        SampleMessage {
            from: correspondents.linus,
            to: vec![Addr::bare("tilo-app@lists.example.test")],
            subject: "Re: [PATCH] fuse: hydrate-on-read for projected blobs",
            body: "On Tue, the patch looks mostly fine, but the readahead window is \
hard-coded to 1MB. Make it a mount option. Also your error path leaks the \
moka entry on a failed fetch — fix that and resend.\n\nNAK until then.",
            ago_min: 26 * HOUR_MIN,
            read: true,
            starred: false,
            folder: Folder::Inbox,
            attachments: vec![],
        }
        .build(),
    );

    // ---- Calendar invite (unread, ics attachment)
    mb.insert(
        SampleMessage {
            from: correspondents.calendar,
            to: vec![me.clone()],
            subject: "Invitation: Compositor sync @ Thu 11:00",
            body: "You have been invited to \"Compositor sync\".\nWhen: Thursday 11:00–\
11:45\nWhere: cluster://review-room\n\nAccept · Decline · Tentative",
            ago_min: 9 * HOUR_MIN,
            read: false,
            starred: false,
            folder: Folder::Inbox,
            attachments: vec![att("invite.ics", "text/calendar", 1_842)],
        }
        .build(),
    );
}

/// The Sent, Archive and Junk folders of the bundled corpus. Split out of
/// [`sample_mailbox`] so neither half runs past the line budget.
fn insert_sent_archive_and_junk(
    mb: &mut Mailbox,
    me: Addr,
    ada: Addr,
    grace: Addr,
    katherine: Addr,
) {
    // ---- Sent items
    mb.insert(
        SampleMessage {
            from: me.clone(),
            to: vec![ada, grace.clone()],
            subject: "Re: Launch plan for the spatial shell",
            body: "Thanks both. I'll own the tiling engine and have a skeleton up by \
Wednesday. Pushing the sandbox policy doc to the shared cluster now.",
            ago_min: 50,
            read: true,
            starred: false,
            folder: Folder::Sent,
            attachments: vec![],
        }
        .build(),
    );
    mb.insert(
        SampleMessage {
            from: me.clone(),
            to: vec![katherine],
            subject: "Re: Re-check the trajectory math",
            body: "Widen it — better a longer window than a missed burn. I'll update \
the solver bounds and re-run tonight.",
            ago_min: 4 * HOUR_MIN + 30,
            read: true,
            starred: false,
            folder: Folder::Sent,
            attachments: vec![],
        }
        .build(),
    );

    // ---- Archive
    mb.insert(
        SampleMessage {
            from: grace,
            to: vec![me.clone()],
            subject: "Onboarding notes",
            body: "Welcome aboard! Here are the cluster conventions and the review \
rota. Ping me if the FUSE mount misbehaves.",
            ago_min: 9 * 1440,
            read: true,
            starred: false,
            folder: Folder::Archive,
            attachments: vec![att("onboarding.pdf", "application/pdf", 512_000)],
        }
        .build(),
    );

    // ---- Junk
    mb.insert(
        SampleMessage {
            from: Addr::new("Totally Legit Prize Co", "winner@prize.example"),
            to: vec![me],
            subject: "\u{1F389} You have WON a free spatial monitor!!!",
            body: "Dear winner, click here within 24 hours to claim your prize. This \
is definitely not a scam.",
            ago_min: 7 * HOUR_MIN,
            read: false,
            starred: false,
            folder: Folder::Junk,
            attachments: vec![],
        }
        .build(),
    );
}

/// A bundled multi-account store so the sidebar shows more than one account
/// offline (`--shots`). The first account owns the sample [`Mailbox`]; the
/// second is a plausible work account (collapsed by default) so the
/// collapsible-per-account sidebar behaviour is visible in a single frame.
pub fn sample_account_store() -> AccountStore {
    let personal = AccountConfig {
        display_name: "You".into(),
        email: "you@example.test".into(),
        imap_host: "imap.example.test".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.example.test".into(),
        smtp_port: 465,
        smtp_tls: true,
        username: "you@example.test".into(),
        auth: crate::model::AuthMethod::Password,
        password: "********".into(),
    };
    let work = AccountConfig {
        display_name: "You at Hooli".into(),
        email: "you@hooli.example".into(),
        imap_host: "imap.hooli.example".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.hooli.example".into(),
        smtp_port: 587,
        smtp_tls: true,
        username: "you@hooli.example".into(),
        auth: crate::model::AuthMethod::Password,
        password: "********".into(),
    };
    let mut a0 = Account::new(personal);
    a0.tint = 0;
    let mut a1 = Account::new(work);
    a1.tint = 3;
    a1.collapsed = true;
    AccountStore {
        accounts: vec![a0, a1],
        active: 0,
    }
}

/// A plain text/plain message, parsed live by mail-parser.
const RAW_PLAIN: &str = "From: Margaret Hamilton <margaret@apollo.example>\r\n\
To: you@example.test\r\n\
Subject: Priority display + error detection\r\n\
Message-ID: <apollo-2026@apollo.example>\r\n\
Date: Tue, 30 Jun 2026 08:30:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
The async-restart logic we built for the guidance computer maps neatly onto \
your compositor's frame scheduler. Drop low-priority redraws under load \
rather than queueing them. Happy to pair on it.\r\n";

/// A multipart/mixed message with a PDF attachment, parsed live.
const RAW_WITH_PDF: &str = "From: Radia Perlman <radia@spanning.example>\r\n\
To: you@example.test\r\n\
Subject: Spanning-tree notes for the gossip mesh\r\n\
Message-ID: <stp-2026@spanning.example>\r\n\
Date: Tue, 30 Jun 2026 07:05:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"=_tilo_boundary_=\"\r\n\
\r\n\
--=_tilo_boundary_=\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Your direct + hosted gossip topology can loop if two clusters bridge the same \
peer. A spanning-tree pass keeps the collaboration buffer from echoing. \
Diagram attached.\r\n\
--=_tilo_boundary_=\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"spanning-tree.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQgZmFrZSBwZGYgYm9keSBmb3IgdGhlIHNhbXBsZSBtYWlsYm94Lg==\r\n\
--=_tilo_boundary_=--\r\n";

/// A multipart/alternative message with a plain-text part and a rich HTML part
/// (including a **remote** image the reader must block by default), parsed live.
const RAW_HTML: &str = "From: Tilo Weekly <news@weekly.example.test>\r\n\
To: you@example.test\r\n\
Subject: This week in Tilo — spatial shells & collab buffers\r\n\
Message-ID: <weekly-2026-27@weekly.example.test>\r\n\
Date: Tue, 30 Jun 2026 13:13:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"=_tilo_alt_=\"\r\n\
\r\n\
--=_tilo_alt_=\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
This week: the scrollable-strip shell landed, and collaboration buffers now \
stream over gossip. Read on.\r\n\
--=_tilo_alt_=\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body style=\"font-family:sans-serif\">\
<h1 style=\"color:#4f7cff\">This week in Tilo</h1>\
<img src=\"https://track.example.test/open.gif?u=you\" width=\"1\" height=\"1\">\
<p>The <strong>scrollable-strip shell</strong> landed, and collaboration \
buffers now stream over gossip.</p>\
<ul><li>Spatial window model</li><li>Per-workspace sandboxing</li>\
<li>Live collab presence</li></ul>\
<blockquote>\u{201c}It feels like one shared desktop.\u{201d}</blockquote>\
<p><a href=\"https://example.test/weekly/27\">Read the full issue \u{2192}</a></p>\
</body></html>\r\n\
--=_tilo_alt_=--\r\n";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::minutes_of;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn sample_corpus_is_populated_threaded_and_hydrated() {
        // Phase `sample_now_matches_civil_date`.
        {
            assert_eq!(SAMPLE_NOW_MIN, minutes_of(2026, 6, 30, 14, 5));
        }

        // Phase `corpus_is_populated_across_folders`.
        {
            let mb = sample_mailbox();
            assert!(
                mb.count(Folder::Inbox) >= 8,
                "inbox: {}",
                mb.count(Folder::Inbox)
            );
            assert!(mb.count(Folder::Sent) >= 2);
            assert!(mb.count(Folder::Archive) >= 1);
            assert!(mb.count(Folder::Junk) >= 1);
            assert!(mb.unread_count(Folder::Inbox) > 0);
        }

        // Phase `parsed_messages_are_present_with_attachment`.
        {
            let mb = sample_mailbox();
            // The Radia Perlman message was parsed from raw MIME and must carry
            // its PDF attachment.
            let radia = mb
                .messages
                .values()
                .find(|m| m.from.email == "radia@spanning.example")
                .expect("parsed Radia message present");
            assert_eq!(radia.attachments.len(), 1);
            assert_eq!(radia.attachments[0].filename, "spanning-tree.pdf");
            // The Margaret message was parsed from plain MIME.
            assert!(
                mb.messages
                    .values()
                    .any(|m| m.from.email == "margaret@apollo.example")
            );
        }

        // Phase `parsed_attachment_is_hydrated_projected_ones_are_not`.
        {
            let mb = sample_mailbox();
            // The Radia message was parsed from raw MIME → real bytes present.
            let radia = mb
                .messages
                .values()
                .find(|m| m.from.email == "radia@spanning.example")
                .unwrap();
            assert!(
                radia.attachments[0].is_downloadable(),
                "parsed attachment carries bytes"
            );
            assert!(!radia.attachments[0].content.is_empty());
            // A hand-built projected attachment has metadata only (not downloaded).
            let gantt = mb
                .messages
                .values()
                .flat_map(|m| m.attachments.iter())
                .find(|a| a.filename == "rollout-gantt.pdf")
                .unwrap();
            assert!(
                !gantt.is_downloadable(),
                "projected attachment has no bytes yet"
            );
        }

        // Phase `threads_collapse_replies`.
        {
            let mb = sample_mailbox();
            let threads = mb.threads_in(Folder::Inbox);
            // The "Launch plan" thread has 3 messages collapsed into one row.
            let launch = threads
                .iter()
                .find(|t| t.subject == "Launch plan for the spatial shell")
                .expect("launch thread present");
            assert_eq!(launch.message_ids.len(), 3);
        }
    }
}
