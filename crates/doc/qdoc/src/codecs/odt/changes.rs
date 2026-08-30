//! Tracked changes ⇄ ODF `text:tracked-changes`. ODF declares every change up front as a
//! `<text:changed-region xml:id="…">` (holding `<text:insertion>`/`<text:deletion>` +
//! `<office:change-info>` author/date), then the body wraps the affected range in
//! matching `<text:change-start>`/`<text:change-end>` markers (or a single empty
//! `<text:change>` for a point change) referencing it by id.
//!
//! This maps onto an `ins`/`del` mark over the body range plus the change's
//! author/date/kind in the `changes` container. qdoc *retains* deleted text in the body
//! (struck, not removed), so a deletion round-trips like an insertion: its retained text
//! is wrapped in change-start/end markers, with the changed-region's `text:deletion`
//! recording only the kind + author/date.

use crate::snapshot::{ChangeKind, ChangeView};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

/// One imported changed-region: the ODF `xml:id`, change kind, and author/date.
pub(super) struct ImportedChange {
    pub xml_id: String,
    pub kind: ChangeKind,
    pub author: String,
    pub date: String,
}

/// Parse a `<text:tracked-changes>` block into its changed-region declarations. The body
/// parser later wires each change-start/-end/change marker (by `xml:id`) onto these.
pub(super) fn import_tracked_changes(raw: &str) -> Vec<ImportedChange> {
    let mut out = Vec::new();
    let mut reader = Reader::from_str(raw);
    let mut cur_id: Option<String> = None;
    let mut cur_kind: Option<ChangeKind> = None;
    let mut author = String::new();
    let mut date = String::new();
    let mut collecting: Option<bool> = None; // Some(true)=dc:creator, Some(false)=dc:date
    loop {
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            Event::Start(e) => match e.name().as_ref() {
                b"text:changed-region" => {
                    cur_id = e
                        .attributes()
                        .flatten()
                        .find(|a| matches!(a.key.as_ref(), b"xml:id" | b"text:id"))
                        .and_then(|a| a.unescape_value().ok().map(std::borrow::Cow::into_owned));
                    cur_kind = None;
                    author.clear();
                    date.clear();
                }
                b"text:insertion" => cur_kind = Some(ChangeKind::Ins),
                b"text:deletion" => cur_kind = Some(ChangeKind::Del),
                b"dc:creator" => collecting = Some(true),
                b"dc:date" => collecting = Some(false),
                _ => {}
            },
            Event::Text(t) => {
                if let (Some(is_author), Ok(txt)) = (collecting, t.xml10_content()) {
                    if is_author {
                        author.push_str(&txt);
                    } else {
                        date.push_str(&txt);
                    }
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"dc:creator" | b"dc:date" => collecting = None,
                b"text:changed-region" => {
                    if let Some(id) = cur_id.take() {
                        out.push(ImportedChange {
                            xml_id: id,
                            kind: cur_kind.take().unwrap_or(ChangeKind::Ins),
                            author: std::mem::take(&mut author),
                            date: std::mem::take(&mut date),
                        });
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Read the `text:change-id` (or `text:id`) attribute of a change marker.
pub(super) fn marker_change_id(e: &quick_xml::events::BytesStart) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| matches!(a.key.as_ref(), b"text:change-id" | b"text:id"))
        .and_then(|a| a.unescape_value().ok().map(std::borrow::Cow::into_owned))
}

fn push_escaped(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(c),
    }
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_escaped(&mut out, c);
    }
    out
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render the `<text:tracked-changes>` block for a document's changes; empty string when there are no changes.
pub(super) fn render_tracked_changes(changes: &[ChangeView]) -> String {
    if changes.is_empty() {
        return String::new();
    }
    let mut out = String::from("<text:tracked-changes>");
    for c in changes {
        let info = render_change_info(c);
        let region = match c.kind {
            ChangeKind::Ins => format!("<text:insertion>{info}</text:insertion>"),
            // A deletion's retained text lives in the body (struck), so the changed-region carries
            // only the change-info; an empty `text:p` keeps the element schema-valid.
            ChangeKind::Del => format!("<text:deletion>{info}<text:p/></text:deletion>"),
        };
        let _ = write!(
            out,
            r#"<text:changed-region xml:id="{}" text:id="{}">{region}</text:changed-region>"#,
            escape_attr(&c.id),
            escape_attr(&c.id)
        );
    }
    out.push_str("</text:tracked-changes>");
    out
}

/// The `<office:change-info>` (author + optional date) for a change.
fn render_change_info(c: &ChangeView) -> String {
    let date = if c.date.is_empty() {
        String::new()
    } else {
        format!("<dc:date>{}</dc:date>", escape_text(&c.date))
    };
    format!(
        "<office:change-info><dc:creator>{}</dc:creator>{date}</office:change-info>",
        escape_text(&c.author)
    )
}

/// The `<text:change-start>` marker opening a change's range in the body.
pub(super) fn render_change_start(id: &str) -> String {
    format!(
        r#"<text:change-start text:change-id="{}"/>"#,
        escape_attr(id)
    )
}

/// The `<text:change-end>` marker closing a change's range in the body.
pub(super) fn render_change_end(id: &str) -> String {
    format!(r#"<text:change-end text:change-id="{}"/>"#, escape_attr(id))
}
