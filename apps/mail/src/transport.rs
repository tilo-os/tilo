//! Transport: the protocol edge of the Mail app.
//!
//! ## The offline / online split (important for `--shots`)
//!
//! `--shots` runs one headless frame with no event loop and no live account,
//! so it cannot talk to an IMAP/SMTP server. To still render a fully populated
//! client we bundle a representative corpus (`sample.rs`). Several messages
//! are authored as raw RFC 5322 `.eml` blobs and **parsed through
//! `mail-parser`** here, matching incoming server messages.
//!
//! Outgoing mail is built with `mail-builder` ([`build_mime`]), producing a
//! multipart RFC 5322 message (headers, text body, and base64 MIME
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
//! headless build; the seam is this module, and the data types
//! ([`AccountConfig`](crate::model::AccountConfig), [`Draft`], [`Message`]) match those libraries' inputs
//! and outputs.

use mail_builder::MessageBuilder;
use mail_parser::{MessageParser, MimeHeaders};
use uuid::Uuid;

use crate::model::{Addr, Attachment, Draft, Folder, Message};

/// Parse a comma/semicolon-separated recipient string (as typed in the
/// compose header fields) into a list of addresses. Tolerates `Name <addr>`
/// and bare `addr` forms.
pub fn parse_addr_list(s: &str) -> Vec<Addr> {
    // Use the same mailbox grammar as incoming headers: punctuation inside
    // a quoted display name is not a recipient separator.
    // The header parser flushes the final token at a line ending, not EOF.
    // Compose fields have no line ending, so supply it for a final bare address.
    let header = format!("{s}\r\n");
    mail_parser::parsers::MessageStream::new(header.as_bytes())
        .parse_address()
        .as_address()
        .map(|addresses| addresses.iter().map(addr_from_parsed).collect())
        .unwrap_or_default()
}

/// Build a real RFC 5322 / MIME message from a draft, via `mail-builder`.
/// Produces the exact bytes an SMTP transport would put on the wire,
/// including a multipart container with base64 attachments when present.
///
/// # Errors
///
/// Returns `Err` if serialisation fails or any portable attachment cannot be
/// materialized and re-verified. In particular, it never silently omits a
/// missing attachment and then reports a successful send.
pub fn build_mime(
    content: &tilo_content::ContentStore,
    from: &Addr,
    draft: &Draft,
) -> Result<Vec<u8>, String> {
    let to = parse_addr_list(&draft.to);
    let cc = parse_addr_list(&draft.cc);

    let mut builder = MessageBuilder::new()
        .from((from.display(), from.email.clone()))
        .subject(draft.subject.clone())
        .text_body(draft.body.clone());
    // A reply names what it answers, so every mail program, this one
    // included, shows it in its conversation.
    if let Some(id) = &draft.wire.message_id {
        builder = builder.message_id(id.clone());
    }
    if let Some(parent) = &draft.wire.in_reply_to {
        builder = builder.in_reply_to(parent.clone());
    }
    if !draft.wire.references.is_empty() {
        builder = builder.references(draft.wire.references.clone());
    }

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
    // Blind recipients belong only in the SMTP envelope. A Bcc header here
    // would disclose them to every recipient of these shared wire bytes.

    for attachment in &draft.attachments {
        let bytes = crate::attachment_store::materialize(content, attachment)?;
        builder = builder.attachment(attachment.mime.clone(), attachment.filename.clone(), bytes);
    }
    if let Some(calendar_ics) = &draft.calendar_ics {
        builder = builder.attachment(
            "text/calendar; method=REQUEST",
            "invitation.ics",
            calendar_ics.as_bytes().to_vec(),
        );
    }

    builder.write_to_vec().map_err(|error| error.to_string())
}

fn addr_tuples(addrs: &[Addr]) -> Vec<(String, String)> {
    addrs
        .iter()
        .map(|a| (a.display(), a.email.clone()))
        .collect()
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
        .map_or_else(|| Addr::bare("unknown@unknown"), addr_from_parsed);

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
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_default();
    // The original text/html part, if the message carried a GENUINE one. The
    // reader renders it sandboxed in a WebView (remote content blocked by
    // default). We match `PartType::Html` explicitly rather than calling
    // `body_html`, because `body_html` auto-converts a text/plain part into HTML
    // as a fallback — which would wrongly route every plain-text mail through
    // the webview. A plain-text mail therefore keeps `html_body == None` and
    // renders as text.
    let html_body = parsed
        .html_bodies()
        .find_map(|part| match &part.body {
            mail_parser::PartType::Html(h) => Some(h.to_string()),
            _ => None,
        })
        .filter(|h| !h.trim().is_empty());

    let rfc_message_id = parsed.message_id().map(std::string::ToString::to_string);
    // RFC linkage is data, not presentation: retain the oldest References
    // token as the canonical root, falling back to In-Reply-To. `References`
    // makes a C→B→A reply stable even if C arrives before B and A; a lone
    // In-Reply-To can only name its immediate parent until more history is
    // loaded. The model normalizes the token before using it as an identity.
    let in_reply_to = parsed
        .references()
        .as_text_list()
        .and_then(|ids| ids.first())
        .map(std::string::ToString::to_string)
        .or_else(|| parsed.in_reply_to().as_text().map(str::to_string));

    let referenced = html_body
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let attachments: Vec<Attachment> = parsed
        .attachments()
        .map(|part| {
            // A part the HTML shows in place (`cid:`) is an image of the
            // message, not a file attached to it.
            let inline_id = part
                .content_id()
                .map(|id| {
                    id.trim()
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_owned()
                })
                .filter(|id| {
                    !id.is_empty()
                        && referenced.contains(&format!("cid:{}", id.to_ascii_lowercase()))
                });
            let filename = part
                .attachment_name()
                .unwrap_or("attachment.bin")
                .to_string();
            let declared = part.content_type().map(|kind| {
                format!("{}/{}", kind.ctype(), kind.subtype().unwrap_or_default())
                    .to_ascii_lowercase()
            });
            let mime = match declared {
                Some(declared) if inline_id.is_some() => declared,
                _ => mime_from_filename(&filename).to_string(),
            };
            // Keep the decoded bytes so the reader can save the real file.
            let content = part.contents().to_vec();
            Attachment {
                size: content.len() as u64,
                mime,
                filename,
                hydrated: !content.is_empty(),
                content_id: None,
                content,
                kept: false,
                inline_id,
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
        in_reply_to,
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
        account: String::new(),
        server: None,
    })
}

/// A deterministic [`Uuid`] for an RFC 5322 `Message-ID`. Hashing the
/// (globally unique) header into the 128-bit id space means the same server
/// message always resolves to the same local id, so a re-fetch dedupes instead
/// of duplicating. Uses the message body's own FNV-1a hash folded into 128 bits
/// — stable across processes and machines.
pub fn stable_message_id(message_id: &str) -> Uuid {
    // Two independent 64-bit FNV-1a passes (different seeds) fill the 128 bits.
    fn fnv1a(bytes: &[u8], key: u64) -> u64 {
        let mut hasher = fnv::FnvHasher::with_key(key);
        std::hash::Hasher::write(&mut hasher, bytes);
        std::hash::Hasher::finish(&hasher)
    }
    let norm = message_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>');
    let hi = fnv1a(norm.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let lo = fnv1a(norm.as_bytes(), 0x0102_0304_0506_0708);
    Uuid::from_u128((u128::from(hi) << 64) | u128::from(lo))
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
    let ts = internal_date_min
        .or(header_date_min)
        .unwrap_or(fallback_min);
    let mut msg = parse_eml(raw, ts, folder)?;
    msg.read = seen;
    msg.starred = flagged;
    Some(msg)
}

fn addr_from_parsed(a: &mail_parser::Addr) -> Addr {
    let name = a
        .name
        .as_ref()
        .map(std::string::ToString::to_string)
        .unwrap_or_default();
    let email = a
        .address
        .as_ref()
        .map(std::string::ToString::to_string)
        .unwrap_or_default();
    Addr::new(name, email)
}

/// Best-effort MIME type from a filename extension, for the attachment glyph
/// and the multipart Content-Type.
pub fn mime_from_filename(name: &str) -> &'static str {
    tilo_mime::from_path_or_octet_stream(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outgoing_mime_does_not_disclose_blind_recipients() {
        let draft = Draft {
            to: "visible@example.test".into(),
            cc: "copy@example.test".into(),
            bcc: "Hidden Person <hidden@example.test>".into(),
            body: "Private recipient list".into(),
            ..Default::default()
        };
        let bytes = build_mime(
            &tilo_content::ContentStore::memory(),
            &Addr::bare("sender@example.test"),
            &draft,
        )
        .unwrap();
        let parsed = MessageParser::default().parse(&bytes).unwrap();
        assert!(parsed.bcc().is_none());
        assert!(!String::from_utf8_lossy(&bytes).contains("hidden@example.test"));
        assert_eq!(
            parsed.to().unwrap().first().unwrap().address(),
            Some("visible@example.test")
        );
        assert_eq!(
            parsed.cc().unwrap().first().unwrap().address(),
            Some("copy@example.test")
        );
    }

    #[test]
    fn mime_build_parses_addresses_and_embeds_verified_attachments() {
        {
            let v = parse_addr_list("Ada Lovelace <ada@x.example>, bob@y.example");
            assert_eq!(v.len(), 2);
            assert_eq!(v[0].name.as_deref(), Some("Ada Lovelace"));
            assert_eq!(v[0].email, "ada@x.example");
            assert_eq!(v[1].email, "bob@y.example");
            assert!(v[1].name.is_none());
        }

        // Phase `build_mime_produces_valid_rfc5322`.
        {
            let content = tilo_content::ContentStore::memory();
            let attachment = crate::attachment_store::stage_bytes(
                &content,
                b"%PDF-1.4 report body",
                "report.pdf",
            )
            .unwrap();
            let from = Addr::new("Me", "me@example.test");
            let draft = Draft {
                to: "you@there.example".into(),
                subject: "Hello".into(),
                body: "Body text here.".into(),
                attachments: vec![attachment],
                ..Default::default()
            };
            let bytes = build_mime(&content, &from, &draft).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(text.contains("Subject: Hello"));
            assert!(text.to_ascii_lowercase().contains("multipart/mixed"));
            assert!(text.contains("me@example.test"));
        }

        {
            let content = tilo_content::ContentStore::memory();
            // Prove the two libraries interoperate: build a message, parse it
            // back, and confirm the headers survive.
            let from = Addr::new("Ada", "ada@analytical.example");
            let draft = Draft {
                to: "you@example.test".into(),
                subject: "Notes from the Analytical Engine".into(),
                body: "The Engine weaves algebraic patterns.".into(),
                ..Default::default()
            };
            let bytes = build_mime(&content, &from, &draft).unwrap();
            let msg = parse_eml(&bytes, 1000, Folder::Inbox).unwrap();
            assert_eq!(msg.subject, "Notes from the Analytical Engine");
            assert_eq!(msg.from.email, "ada@analytical.example");
            assert!(msg.body.contains("algebraic patterns"));
        }

        {
            let content = tilo_content::ContentStore::memory();
            let attachment = crate::attachment_store::stage_bytes(
                &content,
                b"REAL-ATTACHMENT-CONTENT-\x00\x01\x02",
                "report.txt",
            )
            .unwrap();
            let from = Addr::new("Me", "me@example.test");
            let draft = Draft {
                to: "you@there.example".into(),
                subject: "Files".into(),
                body: "See attached.".into(),
                attachments: vec![attachment],
                ..Default::default()
            };
            let bytes = build_mime(&content, &from, &draft).unwrap();
            let parsed = parse_eml(&bytes, 0, Folder::Inbox).unwrap();
            assert_eq!(parsed.attachments.len(), 1);
            assert_eq!(parsed.attachments[0].filename, "report.txt");
            assert_eq!(
                parsed.attachments[0].content,
                b"REAL-ATTACHMENT-CONTENT-\x00\x01\x02"
            );
        }

        {
            let content = tilo_content::ContentStore::memory();
            let draft = Draft {
                to: "guest@example.com".into(),
                subject: "Invitation".into(),
                body: "See attached.".into(),
                calendar_ics: Some("BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n".into()),
                ..Default::default()
            };
            let bytes = build_mime(&content, &Addr::new("Me", "me@example.com"), &draft).unwrap();
            let wire = String::from_utf8_lossy(&bytes);
            assert!(wire.contains("text/calendar; method=REQUEST"), "{wire}");
        }

        {
            let content = tilo_content::ContentStore::memory();
            // A missing blob must not be silently omitted from an otherwise
            // successful-looking send.
            let draft = Draft {
                to: "you@there.example".into(),
                subject: "Nope".into(),
                body: "b".into(),
                attachments: vec![crate::model::DraftAttachment {
                    content_id: format!("mail-attachment:blake3:{}", "0".repeat(64)),
                    filename: "missing.txt".into(),
                    mime: "text/plain".into(),
                    size: 12,
                }],
                ..Default::default()
            };
            assert!(build_mime(&content, &Addr::new("Me", "me@x.example"), &draft).is_err());
        }
    }

    #[test]
    fn parsed_messages_dedupe_and_keep_rfc_thread_linkage() {
        {
            // The confirmed defect: a re-fetch minted a fresh UUID per parse, so
            // the same server message piled up as N copies in the mailbox and the
            // reader drew its header once per copy. A stable id keyed on the
            // Message-ID must collapse them to one.
            let raw = b"From: Ada <ada@x.example>\r\n\
    To: you@example.test\r\n\
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

        {
            let raw = b"From: Ada <ada@x.example>\r\n\
    To: you@example.test\r\n\
    Subject: Re: Launch\r\n\
    Message-ID: <thread-reply@x.example>\r\n\
    In-Reply-To: <thread-root@x.example>\r\n\
    \r\n\
    Reply body.\r\n";
            let message = parse_eml(raw, 1000, Folder::Inbox).expect("valid message");
            assert_eq!(
                message.rfc_message_id.as_deref(),
                Some("thread-reply@x.example")
            );
            assert_eq!(
                message.in_reply_to.as_deref(),
                Some("thread-root@x.example")
            );
        }

        {
            let raw = b"From: Ada <ada@x.example>\r\n\
    To: you@example.test\r\n\
    Subject: Re: Launch\r\n\
    Message-ID: <thread-latest@x.example>\r\n\
    References: <thread-root@x.example> <thread-middle@x.example>\r\n\
    In-Reply-To: <thread-middle@x.example>\r\n\
    \r\n\
    Latest reply.\r\n";
            let message = parse_eml(raw, 1000, Folder::Inbox).expect("valid message");
            assert_eq!(
                message.in_reply_to.as_deref(),
                Some("thread-root@x.example")
            );
        }

        {
            // No Message-ID → a random id, so two genuinely-separate mails without
            // the header don't collapse into one.
            let raw = b"From: a@x.example\r\nSubject: No id\r\n\r\nHi\r\n";
            let a = parse_eml(raw, 1000, Folder::Inbox).unwrap();
            let b = parse_eml(raw, 1000, Folder::Inbox).unwrap();
            assert_ne!(a.id, b.id, "absent Message-ID falls back to a unique id");
        }
    }

    #[test]
    fn parse_eml_extracts_html_and_hydrated_attachments() {
        {
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

        {
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
            assert!(
                att.is_downloadable(),
                "parsed attachment should carry its bytes"
            );
            assert!(att.content.starts_with(b"%PDF-1.4 real body"));
            assert_eq!(att.size, att.content.len() as u64);
        }

        {
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

    #[test]
    fn fetched_messages_overlay_flags_and_prefer_internaldate() {
        {
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
            assert_eq!(
                msg.timestamp_min, 30,
                "Date header (00:30 UTC) → 30 minutes"
            );
            assert_eq!(msg.subject, "Fetched");
        }

        {
            let raw = b"From: a@x.example\r\nSubject: S\r\nDate: Thu, 01 Jan 1970 00:30:00 +0000\r\n\r\nbody\r\n";
            let msg = parse_fetched(raw, Folder::Sent, false, false, Some(120), 999).unwrap();
            assert_eq!(msg.timestamp_min, 120);
            assert!(!msg.read && !msg.starred);
            assert_eq!(msg.folder, Folder::Sent);
        }
    }
}
