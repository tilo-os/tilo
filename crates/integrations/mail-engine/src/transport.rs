//! Transport: the protocol edge of the Mail app.
//!
//! Incoming MIME is parsed with `mail-parser` ([`parse_eml`]) and outgoing mail built with
//! `mail-builder` ([`build_mime`]). The live engine drives IMAP and SMTP on a worker thread;
//! keeping those TLS and async crates out of the headless build is what lets `--shots` render
//! the bundled sample corpus with no server.

use mail_builder::MessageBuilder;
use mail_parser::{MessageParser, MimeHeaders};
use uuid::Uuid;

use crate::model::{Addr, Attachment, Draft, Folder, Message};

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
    let files = draft
        .attachments
        .iter()
        .map(|attachment| {
            let bytes = crate::attachment_store::materialize(content, attachment)?;
            Ok(crate::model::SendFile {
                attachment: attachment.clone(),
                bytes,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    build_mime_from(from, draft, &files)
}

/// Build the message from `draft` and the bytes `files` already carry:
/// the send path of whatever read them, so a page hands the host the bytes
/// it kept and never builds MIME itself.
pub fn build_mime_from(
    from: &Addr,
    draft: &Draft,
    files: &[crate::model::SendFile],
) -> Result<Vec<u8>, String> {
    let to = parse_addr_list(&draft.to);
    let cc = parse_addr_list(&draft.cc);

    // The date it leaves is written from the wall clock this app reads, not
    // the platform's: a page has no platform clock to read, and a message
    // built there is checked before it is sent.
    let sent_at = tilo_clock::unix_now();
    let mut builder = MessageBuilder::new()
        .from((from.display(), from.email.clone()))
        .subject(draft.subject.clone())
        .date(sent_at)
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

    for file in files {
        builder = builder.attachment(
            file.attachment.mime.clone(),
            file.attachment.filename.clone(),
            file.bytes.clone(),
        );
    }
    if let Some(calendar_ics) = &draft.calendar_ics {
        builder = builder.attachment(
            "text/calendar; method=REQUEST",
            "invitation.ics",
            with_organizer(calendar_ics, from).into_bytes(),
        );
    }

    builder.write_to_vec().map_err(|error| error.to_string())
}

/// The invitation with `from` named as the organizer of every event that names none: the
/// people invited answer the account the invitation went from, and mail servers deliver an
/// invitation that says who asked. An organizer the invitation already names is its own.
fn with_organizer(ics: &str, from: &Addr) -> String {
    let name: String = from
        .display()
        .chars()
        .filter(|c| !matches!(c, '"' | '\\' | '\r' | '\n'))
        .collect();
    let organizer = if name.trim().is_empty() || name == from.email {
        format!("ORGANIZER:mailto:{}\r\n", from.email)
    } else {
        format!("ORGANIZER;CN=\"{name}\":mailto:{}\r\n", from.email)
    };
    let mut out = String::with_capacity(ics.len() + organizer.len());
    let mut in_event = false;
    let mut named = false;
    for line in ics.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\r', '\n']);
        if bare.eq_ignore_ascii_case("BEGIN:VEVENT") {
            in_event = true;
            named = false;
        } else if in_event && bare.to_ascii_uppercase().starts_with("ORGANIZER") {
            named = true;
        } else if in_event && bare.eq_ignore_ascii_case("END:VEVENT") {
            if !named {
                out.push_str(&organizer);
            }
            in_event = false;
        }
        out.push_str(line);
    }
    out
}

fn addr_tuples(addrs: &[Addr]) -> Vec<(String, String)> {
    addrs
        .iter()
        .map(|a| (a.display(), a.email.clone()))
        .collect()
}

pub fn parse_eml(raw: &[u8], timestamp_min: i64, folder: Folder) -> Option<Message> {
    parse_eml_apart(raw, timestamp_min, folder, &[])
}

/// [`parse_eml`] of a message some of whose parts are kept apart from it
/// (`apart`: where each one's body goes in `raw`, and the size of its
/// content): those parts' attachments are named with their size and read
/// from where they are kept when opened.
pub fn parse_eml_apart(
    raw: &[u8],
    timestamp_min: i64,
    folder: Folder,
    apart: &[(u64, u64)],
) -> Option<Message> {
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

    // The subject as the message carries it; a window resolves the words for
    // a message with none.
    let subject = parsed.subject().unwrap_or_default().to_string();
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
        .map(|part| attachment_of(part, &referenced, apart))
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

/// The attachment `part` is, in a message whose HTML says `referenced`
/// (lower case) and some of whose parts are kept `apart` (where each one's
/// body goes, and the size of its content).
fn attachment_of(
    part: &mail_parser::MessagePart<'_>,
    referenced: &str,
    apart: &[(u64, u64)],
) -> Attachment {
    // A part the HTML shows in place (`cid:`) is an image of the message,
    // not a file attached to it.
    let inline_id = part
        .content_id()
        .map(|id| {
            id.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_owned()
        })
        .filter(|id| {
            !id.is_empty() && referenced.contains(&format!("cid:{}", id.to_ascii_lowercase()))
        });
    let filename = part
        .attachment_name()
        .unwrap_or("attachment.bin")
        .to_string();
    let declared = part.content_type().map(|kind| {
        format!("{}/{}", kind.ctype(), kind.subtype().unwrap_or_default()).to_ascii_lowercase()
    });
    let mime = match declared {
        Some(declared) if inline_id.is_some() => declared,
        _ => mime_from_filename(&filename).to_string(),
    };
    let content = part.contents().to_vec();
    let kept_apart = apart
        .iter()
        .find(|(at, _)| content.is_empty() && *at == u64::from(part.offset_body))
        .map(|(_, size)| *size);
    Attachment {
        size: kept_apart.unwrap_or(content.len() as u64),
        mime,
        filename,
        hydrated: !content.is_empty() || kept_apart.is_some(),
        content_id: None,
        content,
        kept: false,
        inline_id,
        apart: kept_apart.is_some(),
    }
}

pub fn stable_message_id(message_id: &str) -> Uuid {
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

pub fn mime_from_filename(name: &str) -> &'static str {
    tilo_mime::from_path_or_octet_stream(name)
}
