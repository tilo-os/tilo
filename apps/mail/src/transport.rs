//! Transport: the real protocol edge of the Mail app.
//!
//! ## The offline / online split (important for `--shots`)
//!
//! `--shots` runs one headless frame with no event loop and no live account,
//! so it cannot talk to an IMAP/SMTP server. To still render a fully populated
//! client we bundle a representative corpus (`sample.rs`) — and that corpus is
//! not faked structs typed by hand: several messages are authored as raw
//! RFC 5322 `.eml` blobs and **parsed through `mail-parser`** here, exactly as
//! incoming server messages would be. So the screenshot exercises the real
//! parse path.
//!
//! Outgoing mail is built with `mail-builder` ([`build_mime`]), producing a
//! real multipart RFC 5322 message (headers, text body, and base64 MIME
//! attachments). In the **live** app that byte buffer is handed to an SMTP
//! transport; here we expose it directly so compose/send is genuinely wired
//! end to end up to the socket.
//!
//! ## What the live app does (network path)
//!
//! The live binary (no `--shots`) would drive, on a background worker thread
//! (mirroring the notes `Saver` pattern — the `ui` event loop is synchronous
//! and must not block):
//!
//! * **IMAP** via `async-imap` (or the synchronous `imap` crate): `LOGIN`,
//!   `SELECT INBOX`, `UID SEARCH`, `UID FETCH (ENVELOPE BODY[])`, then feed
//!   each fetched RFC 822 blob into [`parse_eml`]. Flag changes (`\Seen`,
//!   `\Flagged`) and folder moves map onto `UID STORE` / `UID MOVE`.
//! * **SMTP** via `lettre`'s `SmtpTransport` (rustls): connect to
//!   `AccountConfig::smtp_host:smtp_port`, `STARTTLS`/implicit-TLS per
//!   `smtp_tls`, `AUTH`, then submit the [`build_mime`] buffer.
//!
//! Those crates pull a TLS + async stack we deliberately keep out of the
//! headless build so it stays small and green; the seam is this module, and
//! the data types ([`AccountConfig`], [`Draft`], [`Message`]) are already the
//! exact shapes those libraries consume and produce.

use mail_builder::MessageBuilder;
use mail_parser::{MessageParser, MimeHeaders};
use uuid::Uuid;

use crate::model::{Addr, Attachment, Draft, Folder, Message};

/// Parse a comma/semicolon-separated recipient string (as typed in the
/// compose header fields) into a list of addresses. Tolerates `Name <addr>`
/// and bare `addr` forms.
pub fn parse_addr_list(s: &str) -> Vec<Addr> {
    s.split([',', ';'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(parse_single_addr)
        .collect()
}

fn parse_single_addr(s: &str) -> Addr {
    if let Some(open) = s.find('<') {
        if let Some(close) = s[open..].find('>') {
            let name = s[..open].trim().trim_matches('"').to_string();
            let email = s[open + 1..open + close].trim().to_string();
            return Addr::new(name, email);
        }
    }
    Addr::bare(s.to_string())
}

/// Build a real RFC 5322 / MIME message from a draft, via `mail-builder`.
/// Produces the exact bytes an SMTP transport would put on the wire,
/// including a multipart container with base64 attachments when present.
pub fn build_mime(from: &Addr, draft: &Draft) -> std::io::Result<Vec<u8>> {
    let to = parse_addr_list(&draft.to);
    let cc = parse_addr_list(&draft.cc);
    let bcc = parse_addr_list(&draft.bcc);

    let mut builder = MessageBuilder::new()
        .from((from.display(), from.email.clone()))
        .subject(draft.subject.clone())
        .text_body(draft.body.clone());

    // When the draft was authored in WYSIWYG mode, attach the HTML body as the
    // richer alternative. Receivers that render HTML show it; plain-text
    // readers fall back to `text_body`. `mail-builder` assembles the
    // `multipart/alternative` (and `multipart/mixed` when attachments are
    // present) container for us.
    if let Some(html) = &draft.html_body {
        builder = builder.html_body(html.clone());
    }

    if !to.is_empty() {
        builder = builder.to(addr_tuples(&to));
    }
    if !cc.is_empty() {
        builder = builder.cc(addr_tuples(&cc));
    }
    if !bcc.is_empty() {
        builder = builder.bcc(addr_tuples(&bcc));
    }

    // Attachments are staged as real filesystem paths (the FUSE-projected file
    // the user picked). Read the actual bytes so the produced message carries
    // the genuine file — never a fabricated placeholder. A path that can't be
    // read is skipped rather than sending fake content.
    for path in &draft.attachments {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let name = crate::model::attachment_display_name(path).to_string();
        let mime = mime_from_filename(&name);
        builder = builder.attachment(mime, name, bytes);
    }

    builder.write_to_vec()
}

fn addr_tuples(addrs: &[Addr]) -> Vec<(String, String)> {
    addrs.iter().map(|a| (a.display(), a.email.clone())).collect()
}

/// Parse a raw RFC 5322 message into our model via `mail-parser`. The
/// timestamp and target folder are supplied by the caller (in the live app
/// the folder is the IMAP mailbox being fetched; the timestamp comes from
/// the `Date:` header / `INTERNALDATE`).
pub fn parse_eml(raw: &[u8], timestamp_min: i64, folder: Folder) -> Option<Message> {
    let parsed = MessageParser::default().parse(raw)?;

    let from = parsed
        .from()
        .and_then(|a| a.first())
        .map(addr_from_parsed)
        .unwrap_or_else(|| Addr::bare("unknown@unknown"));

    let to = parsed
        .to()
        .map(|a| a.iter().map(addr_from_parsed).collect())
        .unwrap_or_default();
    let cc = parsed
        .cc()
        .map(|a| a.iter().map(addr_from_parsed).collect())
        .unwrap_or_default();

    let subject = parsed.subject().unwrap_or("(no subject)").to_string();
    let body = parsed
        .body_text(0)
        .map(|c| c.into_owned())
        .unwrap_or_default();
    // The original text/html part, if the message carried a GENUINE one. The
    // reader renders it natively (sanitized rich text + images, remote content
    // blocked by default). We match `PartType::Html` explicitly rather than
    // calling `body_html`, because `body_html` auto-converts a text/plain part
    // into HTML as a fallback — which would wrongly route every plain-text mail
    // through the HTML path. A plain-text mail therefore keeps `html_body ==
    // None` and renders as text.
    let html_body = parsed
        .html_bodies()
        .find_map(|part| match &part.body {
            mail_parser::PartType::Html(h) => Some(h.to_string()),
            _ => None,
        })
        .filter(|h| !h.trim().is_empty());

    let rfc_message_id = parsed.message_id().map(|s| s.to_string());

    let attachments: Vec<Attachment> = parsed
        .attachments()
        .map(|part| {
            let filename = part
                .attachment_name()
                .unwrap_or("attachment.bin")
                .to_string();
            let mime = mime_from_filename(&filename).to_string();
            // Keep the decoded bytes so the reader can save the real file.
            let content = part.contents().to_vec();
            Attachment {
                size: content.len() as u64,
                mime,
                filename,
                hydrated: !content.is_empty(),
                content,
            }
        })
        .collect();

    // Derive a STABLE id from the RFC `Message-ID` so that re-fetching the same
    // server message (initial connect, folder switch, periodic refresh) maps to
    // the same local message and overwrites in place, rather than piling up N
    // duplicates keyed by fresh random UUIDs — which is what made an opened mail
    // render its header several times, stacked. Only mint a random id when the
    // message genuinely has no `Message-ID`.
    let id = match &rfc_message_id {
        Some(mid) if !mid.trim().is_empty() => stable_message_id(mid),
        _ => Uuid::new_v4(),
    };

    Some(Message {
        id,
        rfc_message_id,
        in_reply_to: None,
        from,
        to,
        cc,
        subject,
        body,
        html_body,
        attachments,
        timestamp_min,
        read: false,
        starred: false,
        folder,
        labels: Vec::new(),
    })
}

/// A deterministic [`Uuid`] for an RFC 5322 `Message-ID`. Hashing the
/// (globally unique) header into the 128-bit id space means the same server
/// message always resolves to the same local id, so a re-fetch dedupes instead
/// of duplicating. Uses the message body's own FNV-1a hash folded into 128 bits
/// — no crate needed, and stable across processes and machines.
pub fn stable_message_id(message_id: &str) -> Uuid {
    // Two independent 64-bit FNV-1a passes (different seeds) fill the 128 bits.
    fn fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
        const PRIME: u64 = 0x0000_0100_0000_01B3;
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(PRIME);
        }
        hash
    }
    let norm = message_id.trim().trim_start_matches('<').trim_end_matches('>');
    let hi = fnv1a(norm.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let lo = fnv1a(norm.as_bytes(), 0x0102_0304_0506_0708);
    Uuid::from_u128(((hi as u128) << 64) | lo as u128)
}

/// Parse a raw RFC 5322 message fetched over IMAP, carrying the server's flag
/// state and internal date. Reuses [`parse_eml`] for the header/body/attachment
/// extraction, then overlays the `\Seen`/`\Flagged` state and the message
/// timestamp. `internal_date_min` is the IMAP `INTERNALDATE` (minutes since the
/// epoch) when the server supplied one; otherwise the message's own `Date:`
/// header is used, falling back to `fallback_min`.
pub fn parse_fetched(
    raw: &[u8],
    folder: Folder,
    seen: bool,
    flagged: bool,
    internal_date_min: Option<i64>,
    fallback_min: i64,
) -> Option<Message> {
    let header_date_min = MessageParser::default()
        .parse(raw)
        .and_then(|p| p.date().map(|d| d.to_timestamp() / 60));
    let ts = internal_date_min.or(header_date_min).unwrap_or(fallback_min);
    let mut msg = parse_eml(raw, ts, folder)?;
    msg.read = seen;
    msg.starred = flagged;
    Some(msg)
}

fn addr_from_parsed(a: &mail_parser::Addr) -> Addr {
    let name = a.name.as_ref().map(|c| c.to_string()).unwrap_or_default();
    let email = a
        .address
        .as_ref()
        .map(|c| c.to_string())
        .unwrap_or_default();
    Addr::new(name, email)
}

/// Best-effort MIME type from a filename extension. Small static table —
/// enough for the attachment glyph + the multipart Content-Type.
pub fn mime_from_filename(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "ics" => "text/calendar",
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "doc" | "docx" => "application/msword",
        "xls" | "xlsx" => "application/vnd.ms-excel",
        "ppt" | "pptx" => "application/vnd.ms-powerpoint",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Outcome of a (simulated, in this slice) send. The live app returns the
/// SMTP server's reply here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// Bytes were built and (in the live app) submitted. Carries the wire
    /// size so the UI can confirm "sent (N KB)".
    Sent { wire_bytes: usize },
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_addr_list_handles_named_and_bare() {
        let v = parse_addr_list("Ada Lovelace <ada@x.example>, bob@y.example");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(v[0].email, "ada@x.example");
        assert_eq!(v[1].email, "bob@y.example");
        assert!(v[1].name.is_none());
    }

    #[test]
    fn build_mime_produces_valid_rfc5322() {
        // Attachments are real file paths — stage one so the message is a
        // genuine multipart/mixed with the file's real bytes.
        let path = std::env::temp_dir().join(format!("mail-report-{}.pdf", Uuid::new_v4()));
        std::fs::write(&path, b"%PDF-1.4 report body").unwrap();
        let from = Addr::new("Me", "me@qos.example");
        let draft = Draft {
            to: "you@there.example".into(),
            subject: "Hello".into(),
            body: "Body text here.".into(),
            attachments: vec![path.to_string_lossy().into_owned()],
            ..Default::default()
        };
        let bytes = build_mime(&from, &draft).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Subject: Hello"));
        assert!(text.to_ascii_lowercase().contains("multipart/mixed"));
        assert!(text.contains("me@qos.example"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn refetching_the_same_message_id_dedupes_not_duplicates() {
        // The confirmed defect: a re-fetch minted a fresh UUID per parse, so
        // the same server message piled up as N copies in the mailbox and the
        // reader drew its header once per copy. A stable id keyed on the
        // Message-ID must collapse them to one.
        let raw = b"From: Ada <ada@x.example>\r\n\
To: you@qos.example\r\n\
Subject: Re: Launch\r\n\
Message-ID: <thread-42@x.example>\r\n\
\r\n\
Body of the message.\r\n";
        let a = parse_eml(raw, 1000, Folder::Inbox).unwrap();
        let b = parse_eml(raw, 2000, Folder::Inbox).unwrap();
        assert_eq!(a.id, b.id, "same Message-ID → same local id across fetches");

        let mut mb = crate::model::Mailbox::default();
        mb.insert(a);
        mb.insert(b);
        assert_eq!(
            mb.messages_in(Folder::Inbox).len(),
            1,
            "re-inserting the same server message must not duplicate it"
        );
    }

    #[test]
    fn messages_without_a_message_id_stay_distinct() {
        // No Message-ID → a random id, so two genuinely-separate mails without
        // the header don't collapse into one.
        let raw = b"From: a@x.example\r\nSubject: No id\r\n\r\nHi\r\n";
        let a = parse_eml(raw, 1000, Folder::Inbox).unwrap();
        let b = parse_eml(raw, 1000, Folder::Inbox).unwrap();
        assert_ne!(a.id, b.id, "absent Message-ID falls back to a unique id");
    }

    #[test]
    fn parse_eml_captures_the_html_part() {
        let raw = b"From: a@x.example\r\n\
Subject: Rich\r\n\
Message-ID: <html-1@x.example>\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body><h1>Hi</h1><p>Rich <b>content</b>.</p></body></html>\r\n";
        let msg = parse_eml(raw, 1000, Folder::Inbox).unwrap();
        assert!(msg.html_body.is_some(), "an HTML mail keeps its html part");
        assert!(msg.html_body.as_deref().unwrap().contains("<h1>"));
        // A stripped text fallback is still available for the list snippet.
        assert!(msg.body.to_ascii_lowercase().contains("hi"));
    }

    #[test]
    fn build_then_parse_round_trips_headers() {
        // Prove the two libraries interoperate: build a message, parse it
        // back, and confirm the headers survive.
        let from = Addr::new("Ada", "ada@analytical.example");
        let draft = Draft {
            to: "you@qos.example".into(),
            subject: "Notes from the Analytical Engine".into(),
            body: "The Engine weaves algebraic patterns.".into(),
            ..Default::default()
        };
        let bytes = build_mime(&from, &draft).unwrap();
        let msg = parse_eml(&bytes, 1000, Folder::Inbox).unwrap();
        assert_eq!(msg.subject, "Notes from the Analytical Engine");
        assert_eq!(msg.from.email, "ada@analytical.example");
        assert!(msg.body.contains("algebraic patterns"));
    }

    #[test]
    fn build_mime_embeds_real_attachment_bytes_from_path() {
        // Stage a real file, attach it by path, and confirm the produced
        // message carries the genuine bytes (no fabricated placeholder).
        let path = std::env::temp_dir().join(format!("mail-att-{}.txt", Uuid::new_v4()));
        std::fs::write(&path, b"REAL-ATTACHMENT-CONTENT-\x00\x01\x02").unwrap();
        let from = Addr::new("Me", "me@qos.example");
        let draft = Draft {
            to: "you@there.example".into(),
            subject: "Files".into(),
            body: "See attached.".into(),
            attachments: vec![path.to_string_lossy().into_owned()],
            ..Default::default()
        };
        let bytes = build_mime(&from, &draft).unwrap();
        let parsed = parse_eml(&bytes, 0, Folder::Inbox).unwrap();
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename, path.file_name().unwrap().to_string_lossy());
        assert_eq!(parsed.attachments[0].content, b"REAL-ATTACHMENT-CONTENT-\x00\x01\x02");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn build_mime_skips_unreadable_attachment_path() {
        // A path that can't be read must not fabricate an attachment.
        let draft = Draft {
            to: "you@there.example".into(),
            subject: "Nope".into(),
            body: "b".into(),
            attachments: vec!["/no/such/mail/file-definitely-missing".into()],
            ..Default::default()
        };
        let bytes = build_mime(&Addr::new("Me", "me@x.example"), &draft).unwrap();
        let parsed = parse_eml(&bytes, 0, Folder::Inbox).unwrap();
        assert!(parsed.attachments.is_empty(), "unreadable path must not emit an attachment");
    }

    #[test]
    fn parse_eml_hydrates_attachment_content() {
        let raw = b"From: A <a@x.example>\r\n\
To: b@y.example\r\n\
Subject: With file\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"BOUND\"\r\n\
\r\n\
--BOUND\r\n\
Content-Type: text/plain\r\n\
\r\n\
See attached.\r\n\
--BOUND\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"spec.pdf\"\r\n\
\r\n\
%PDF-1.4 real body\r\n\
--BOUND--\r\n";
        let msg = parse_eml(raw, 42, Folder::Inbox).unwrap();
        let att = &msg.attachments[0];
        assert!(att.is_downloadable(), "parsed attachment should carry its bytes");
        assert!(att.content.starts_with(b"%PDF-1.4 real body"));
        assert_eq!(att.size, att.content.len() as u64);
    }

    #[test]
    fn parse_fetched_overlays_flags_and_uses_header_date() {
        // A real fetched blob: the \Seen/\Flagged state comes from IMAP FLAGS,
        // and with no INTERNALDATE the timestamp falls back to the Date header.
        let raw = b"From: A <a@x.example>\r\n\
To: b@y.example\r\n\
Subject: Fetched\r\n\
Date: Thu, 01 Jan 1970 00:30:00 +0000\r\n\
\r\n\
Hello from the server.\r\n";
        let msg = parse_fetched(raw, Folder::Inbox, true, true, None, 999).unwrap();
        assert!(msg.read, "\\Seen maps to read");
        assert!(msg.starred, "\\Flagged maps to starred");
        assert_eq!(msg.timestamp_min, 30, "Date header (00:30 UTC) → 30 minutes");
        assert_eq!(msg.subject, "Fetched");
    }

    #[test]
    fn parse_fetched_prefers_internaldate_over_header() {
        let raw = b"From: a@x.example\r\nSubject: S\r\nDate: Thu, 01 Jan 1970 00:30:00 +0000\r\n\r\nbody\r\n";
        // INTERNALDATE wins when the server supplied one.
        let msg = parse_fetched(raw, Folder::Sent, false, false, Some(120), 999).unwrap();
        assert_eq!(msg.timestamp_min, 120);
        assert!(!msg.read && !msg.starred);
        assert_eq!(msg.folder, Folder::Sent);
    }

    #[test]
    fn parse_eml_extracts_attachment() {
        let raw = b"From: A <a@x.example>\r\n\
To: b@y.example\r\n\
Subject: With file\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"BOUND\"\r\n\
\r\n\
--BOUND\r\n\
Content-Type: text/plain\r\n\
\r\n\
See attached.\r\n\
--BOUND\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"spec.pdf\"\r\n\
\r\n\
%PDF-1.4 fake\r\n\
--BOUND--\r\n";
        let msg = parse_eml(raw, 42, Folder::Inbox).unwrap();
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].filename, "spec.pdf");
        assert_eq!(msg.attachments[0].mime, "application/pdf");
        assert!(msg.body.contains("See attached"));
    }
}
