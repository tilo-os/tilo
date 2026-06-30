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

    // Stage attachments as small placeholder payloads. In the live app the
    // bytes come from the FUSE-projected file the user attached; here we
    // emit a representative body so the produced message is structurally a
    // real multipart/mixed.
    for name in &draft.attachments {
        let mime = mime_from_filename(name);
        let payload = format!("Attachment placeholder for {name}\n").into_bytes();
        builder = builder.attachment(mime, name.clone(), payload);
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

    let rfc_message_id = parsed.message_id().map(|s| s.to_string());

    let attachments: Vec<Attachment> = parsed
        .attachments()
        .map(|part| {
            let filename = part
                .attachment_name()
                .unwrap_or("attachment.bin")
                .to_string();
            let mime = mime_from_filename(&filename).to_string();
            Attachment {
                size: part.len() as u64,
                mime,
                filename,
                hydrated: true,
            }
        })
        .collect();

    Some(Message {
        id: Uuid::new_v4(),
        rfc_message_id,
        in_reply_to: None,
        from,
        to,
        cc,
        subject,
        body,
        attachments,
        timestamp_min,
        read: false,
        starred: false,
        folder,
    })
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
        let from = Addr::new("Me", "me@qos.example");
        let draft = Draft {
            to: "you@there.example".into(),
            subject: "Hello".into(),
            body: "Body text here.".into(),
            attachments: vec!["report.pdf".into()],
            ..Default::default()
        };
        let bytes = build_mime(&from, &draft).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Subject: Hello"));
        assert!(text.to_ascii_lowercase().contains("multipart/mixed"));
        assert!(text.contains("me@qos.example"));
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
