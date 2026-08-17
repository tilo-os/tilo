//! A representative, fully populated sample mailbox used when the app runs
//! headless (`--shots`) or before a live account is configured. It is *not*
//! a hollow mockup: two messages (including one with a PDF attachment) are
//! authored as raw RFC 5322 `.eml` blobs and hydrated through
//! [`crate::transport::parse_eml`], so the screenshot exercises the real
//! MIME-parse path. The rest are constructed directly for breadth across
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
        html_body: None,
        attachments,
        timestamp_min: SAMPLE_NOW_MIN - ago_min,
        read,
        starred,
        folder,
        labels: Vec::new(),
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
        auth: crate::model::AuthMethod::Password,
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
    // A rich HTML newsletter — an inline `data:` banner (renders offline), a
    // remote screenshot (blocked until the reader opts in), and a 1×1 tracking
    // pixel (silently dropped) — so the reader exercises the native HTML render
    // path and the remote-content block.
    if let Some(mut m) = parse_eml(raw_html_newsletter().as_bytes(), SAMPLE_NOW_MIN - 52, Folder::Inbox) {
        m.read = false;
        m.starred = true;
        // Use the pristine body (with the inline banner) — see `newsletter_html_body`.
        m.html_body = Some(newsletter_html_body());
        mb.insert(m);
    }

    mb
}

/// A bundled multi-account store so the sidebar shows more than one account
/// offline (`--shots`). The first account owns the sample [`Mailbox`]; the
/// second is a plausible work account (collapsed by default) so the
/// collapsible-per-account sidebar behaviour is visible in a single frame.
pub fn sample_account_store() -> AccountStore {
    let personal = AccountConfig {
        display_name: "You".into(),
        email: "you@qos.example".into(),
        imap_host: "imap.qos.example".into(),
        imap_port: 993,
        imap_tls: true,
        smtp_host: "smtp.qos.example".into(),
        smtp_port: 465,
        smtp_tls: true,
        username: "you@qos.example".into(),
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
    AccountStore { accounts: vec![a0, a1], active: 0 }
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

/// The newsletter EML — real headers + a plain-text part + the HTML part, all
/// parsed live by `mail-parser`. The HTML body carries a remote screenshot
/// (blocked until the reader opts in) and a 1×1 tracking pixel (dropped).
fn raw_html_newsletter() -> String {
    format!("{MIME_HEAD}{HTML_OPEN}{HTML_TAIL}\r\n--=_qos_alt_=--\r\n")
}

/// The pristine HTML body **with** the inline `data:` banner. A hand-authored
/// 2 KB single-line data: URI does not survive a round-trip through the MIME
/// line handling intact, so the sample sets this genuine body directly rather
/// than reading mail-parser's mangled copy. Real IMAP mail is unaffected — its
/// bodies come straight off the wire.
fn newsletter_html_body() -> String {
    format!(
        "{HTML_OPEN}<img src=\"{}\" width=\"360\" height=\"96\" alt=\"qOS Weekly banner\">{HTML_TAIL}",
        banner_data_uri()
    )
}

/// The banner PNG as base64, kept in its own file (`sample_banner.b64`) so a
/// 2 KB literal can't be mis-transcribed; decoded on the fly by the reader's
/// image store.
fn banner_data_uri() -> String {
    format!("data:image/png;base64,{}", include_str!("sample_banner.b64").trim())
}

const MIME_HEAD: &str = "From: qOS Weekly <news@weekly.qos.example>\r\n\
To: you@qos.example\r\n\
Subject: This week in qOS — spatial shells & collab buffers\r\n\
Message-ID: <weekly-2026-27@weekly.qos.example>\r\n\
Date: Tue, 30 Jun 2026 13:13:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"=_qos_alt_=\"\r\n\
\r\n\
--=_qos_alt_=\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
This week: the scrollable-strip shell landed, and collaboration buffers now \
stream over gossipsub. Read on.\r\n\
--=_qos_alt_=\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n";

const HTML_OPEN: &str = "<html><body style=\"font-family:sans-serif\">\
<h1 style=\"color:#4f7cff\">This week in qOS</h1>";

const HTML_TAIL: &str = "\
<img src=\"https://track.weekly.qos.example/open.gif?u=you\" width=\"1\" height=\"1\">\
<p>The <strong>scrollable-strip shell</strong> landed, and collaboration \
buffers now stream over gossipsub.</p>\
<ul><li>Spatial window model</li><li>Per-workspace sandboxing</li>\
<li>Live collab presence</li></ul>\
<blockquote>\u{201c}It feels like one shared desktop.\u{201d}</blockquote>\
<img src=\"https://cdn.weekly.qos.example/shell.png\" width=\"320\" height=\"150\" alt=\"The scrollable-strip shell\">\
<p><a href=\"https://qos.example/weekly/27\">Read the full issue \u{2192}</a></p>\
</body></html>";

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
    fn parsed_attachment_is_hydrated_projected_ones_are_not() {
        let mb = sample_mailbox();
        // The Radia message was parsed from raw MIME → real bytes present.
        let radia = mb
            .messages
            .values()
            .find(|m| m.from.email == "radia@spanning.example")
            .unwrap();
        assert!(radia.attachments[0].is_downloadable(), "parsed attachment carries bytes");
        assert!(!radia.attachments[0].content.is_empty());
        // A hand-built projected attachment has metadata only (not downloaded).
        let gantt = mb
            .messages
            .values()
            .flat_map(|m| m.attachments.iter())
            .find(|a| a.filename == "rollout-gantt.pdf")
            .unwrap();
        assert!(!gantt.is_downloadable(), "projected attachment has no bytes yet");
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
