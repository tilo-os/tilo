//! A representative, fully populated sample mailbox used when the app runs
//! headless (`--shots`) or before a live account is configured. It is *not*
//! a hollow mockup: two messages (including one with a PDF attachment) are
//! authored as raw RFC 5322 `.eml` blobs and hydrated through
//! [`crate::transport::parse_eml`], so the screenshot exercises the real
//! MIME-parse path. The rest are constructed directly for breadth across
//! folders, threads, read/unread, and starred state.

use uuid::Uuid;

use crate::model::{AccountConfig, Addr, Attachment, Folder, Mailbox, Message};
use crate::transport::parse_eml;

/// Deterministic "now" for the sample corpus so relative timestamps render
/// identically every run. 2026-06-30 14:05 UTC.
pub const SAMPLE_NOW_MIN: i64 = {
    // Equals util::minutes_of(2026, 6, 30, 14, 5); pinned by a test so the
    // literal can never silently drift from the civil-date math.
    29_713_805
};

fn att(filename: &str, mime: &str, size: u64) -> Attachment {
    Attachment { filename: filename.into(), mime: mime.into(), size, hydrated: true }
}

#[allow(clippy::too_many_arguments)]
fn message(
    from: Addr,
    to: Vec<Addr>,
    subject: &str,
    body: &str,
    ago_min: i64,
    read: bool,
    starred: bool,
    folder: Folder,
    attachments: Vec<Attachment>,
) -> Message {
    Message {
        id: Uuid::new_v4(),
        rfc_message_id: None,
        in_reply_to: None,
        from,
        to,
        cc: vec![],
        subject: subject.into(),
        body: body.into(),
        attachments,
        timestamp_min: SAMPLE_NOW_MIN - ago_min,
        read,
        starred,
        folder,
    }
}

/// Build the bundled mailbox. The account is a plausible self-host config so
/// the account-setup screen has real values to display.
pub fn sample_mailbox() -> Mailbox {
    let me = Addr::new("You", "you@qos.example");
    let account = AccountConfig {
        display_name: "You".into(),
        email: "you@qos.example".into(),
        imap_host: "imap.qos.example".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.qos.example".into(),
        smtp_port: 465,
        smtp_tls: true,
        username: "you@qos.example".into(),
        password: "********".into(),
    };
    let mut mb = Mailbox::new(account);

    let ada = Addr::new("Ada Lovelace", "ada@analytical.example");
    let grace = Addr::new("Grace Hopper", "grace@navy.example");
    let alan = Addr::new("Alan Turing", "alan@bletchley.example");
    let katherine = Addr::new("Katherine Johnson", "katherine@nasa.example");
    let linus = Addr::new("Linus Torvalds", "linus@kernel.example");
    let ci = Addr::new("qOS CI", "ci@qos.example");
    let calendar = Addr::new("Calendar", "noreply@cal.qos.example");

    // ---- Thread 1: "Launch plan for the spatial shell" (3 msgs, unread head)
    let h = 60; // an hour
    mb.insert(message(
        ada.clone(),
        vec![me.clone()],
        "Launch plan for the spatial shell",
        "Hi team,\n\nI sketched the rollout for the 2D-cluster window model. \
First milestone is the tiling engine, then per-cluster sandboxing, then the \
collaboration buffer streaming. Thoughts on sequencing?\n\n— Ada",
        2 * h + 23,
        true,
        true,
        Folder::Inbox,
        vec![],
    ));
    mb.insert(message(
        grace.clone(),
        vec![me.clone(), ada.clone()],
        "Re: Launch plan for the spatial shell",
        "Sequencing looks right. I'd pull sandboxing forward — it's the \
security boundary everything else leans on. Let's not ship streaming before \
the bubblewrap policy is solid.\n\nGrace",
        1 * h + 47,
        true,
        false,
        Folder::Inbox,
        vec![],
    ));
    mb.insert(message(
        ada.clone(),
        vec![me.clone(), grace.clone()],
        "Re: Launch plan for the spatial shell",
        "Agreed. New order: tiling → sandbox → streaming. I attached the \
revised gantt. Can you review the collaboration section before Friday?",
        38,
        false,
        false,
        Folder::Inbox,
        vec![att("rollout-gantt.pdf", "application/pdf", 184_320)],
    ));

    // ---- Thread 2: CI failure (unread, attachment: log)
    mb.insert(message(
        ci.clone(),
        vec![me.clone()],
        "Build #4821 failed on shell --lib",
        "Pipeline qos/qos run #4821 FAILED.\n\nstage: cargo test -p shell --lib\n\
error: process didn't exit successfully: SIGSEGV (parallel wgpu instance \
contention?)\n\nFull log attached. Re-run threshold was 4 OK / 8 SIGSEGV.",
        14,
        false,
        false,
        Folder::Inbox,
        vec![att("build-4821.log", "text/plain", 28_716)],
    ));

    // ---- Thread 3: Katherine, calculations (read)
    mb.insert(message(
        katherine.clone(),
        vec![me.clone()],
        "Re-check the trajectory math",
        "I ran the re-entry numbers by hand and they line up with the \
solver. Margins are tight on the third burn — want me to widen the window?",
        5 * h,
        true,
        true,
        Folder::Inbox,
        vec![att("trajectory.csv", "text/csv", 9_204)],
    ));

    // ---- Thread 4: Alan, single short (unread)
    mb.insert(message(
        alan.clone(),
        vec![me.clone()],
        "Coffee before the standup?",
        "Fancy a coffee at 9:15 before we dive into the compositor review? \
I have thoughts on the focus model.",
        3,
        false,
        false,
        Folder::Inbox,
        vec![],
    ));

    // ---- Thread 5: Linus, mailing-list style (read, long)
    mb.insert(message(
        linus.clone(),
        vec![Addr::bare("qos-dev@lists.qos.example")],
        "Re: [PATCH] fuse: hydrate-on-read for projected blobs",
        "On Tue, the patch looks mostly fine, but the readahead window is \
hard-coded to 1MB. Make it a mount option. Also your error path leaks the \
moka entry on a failed fetch — fix that and resend.\n\nNAK until then.",
        26 * h,
        true,
        false,
        Folder::Inbox,
        vec![],
    ));

    // ---- Calendar invite (unread, ics attachment)
    mb.insert(message(
        calendar.clone(),
        vec![me.clone()],
        "Invitation: Compositor sync @ Thu 11:00",
        "You have been invited to \"Compositor sync\".\nWhen: Thursday 11:00–\
11:45\nWhere: cluster://review-room\n\nAccept · Decline · Tentative",
        9 * h,
        false,
        false,
        Folder::Inbox,
        vec![att("invite.ics", "text/calendar", 1_842)],
    ));

    // ---- Sent items
    mb.insert(message(
        me.clone(),
        vec![ada.clone(), grace.clone()],
        "Re: Launch plan for the spatial shell",
        "Thanks both. I'll own the tiling engine and have a skeleton up by \
Wednesday. Pushing the sandbox policy doc to the shared cluster now.",
        50,
        true,
        false,
        Folder::Sent,
        vec![],
    ));
    mb.insert(message(
        me.clone(),
        vec![katherine.clone()],
        "Re: Re-check the trajectory math",
        "Widen it — better a longer window than a missed burn. I'll update \
the solver bounds and re-run tonight.",
        4 * h + 30,
        true,
        false,
        Folder::Sent,
        vec![],
    ));

    // ---- Archive
    mb.insert(message(
        grace.clone(),
        vec![me.clone()],
        "Onboarding notes",
        "Welcome aboard! Here are the cluster conventions and the review \
rota. Ping me if the FUSE mount misbehaves.",
        9 * 1440,
        true,
        false,
        Folder::Archive,
        vec![att("onboarding.pdf", "application/pdf", 512_000)],
    ));

    // ---- Junk
    mb.insert(message(
        Addr::new("Totally Legit Prize Co", "winner@prize.example"),
        vec![me.clone()],
        "\u{1F389} You have WON a free spatial monitor!!!",
        "Dear winner, click here within 24 hours to claim your prize. This \
is definitely not a scam.",
        7 * h,
        false,
        false,
        Folder::Junk,
        vec![],
    ));

    // ---- Two messages hydrated through the REAL mail-parser path ----
    // (one plain, one multipart/mixed with a PDF attachment). This proves
    // the offline screenshot is running the same parse code the live IMAP
    // fetch would.
    if let Some(mut m) = parse_eml(RAW_PLAIN.as_bytes(), SAMPLE_NOW_MIN - 30, Folder::Inbox) {
        m.read = false;
        m.starred = false;
        mb.insert(m);
    }
    if let Some(mut m) = parse_eml(RAW_WITH_PDF.as_bytes(), SAMPLE_NOW_MIN - 7 * 60, Folder::Inbox)
    {
        m.read = true;
        mb.insert(m);
    }

    mb
}

/// A plain text/plain message, parsed live by mail-parser.
const RAW_PLAIN: &str = "From: Margaret Hamilton <margaret@apollo.example>\r\n\
To: you@qos.example\r\n\
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
To: you@qos.example\r\n\
Subject: Spanning-tree notes for the gossip mesh\r\n\
Message-ID: <stp-2026@spanning.example>\r\n\
Date: Tue, 30 Jun 2026 07:05:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"=_qos_boundary_=\"\r\n\
\r\n\
--=_qos_boundary_=\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Your mdns + gossipsub topology can loop if two clusters bridge the same \
peer. A spanning-tree pass keeps the collaboration buffer from echoing. \
Diagram attached.\r\n\
--=_qos_boundary_=\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"spanning-tree.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQgZmFrZSBwZGYgYm9keSBmb3IgdGhlIHNhbXBsZSBtYWlsYm94Lg==\r\n\
--=_qos_boundary_=--\r\n";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::minutes_of;

    #[test]
    fn sample_now_matches_civil_date() {
        assert_eq!(SAMPLE_NOW_MIN, minutes_of(2026, 6, 30, 14, 5));
    }

    #[test]
    fn corpus_is_populated_across_folders() {
        let mb = sample_mailbox();
        assert!(mb.count(Folder::Inbox) >= 8, "inbox: {}", mb.count(Folder::Inbox));
        assert!(mb.count(Folder::Sent) >= 2);
        assert!(mb.count(Folder::Archive) >= 1);
        assert!(mb.count(Folder::Junk) >= 1);
        assert!(mb.unread_count(Folder::Inbox) > 0);
    }

    #[test]
    fn parsed_messages_are_present_with_attachment() {
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
        assert!(mb
            .messages
            .values()
            .any(|m| m.from.email == "margaret@apollo.example"));
    }

    #[test]
    fn threads_collapse_replies() {
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
