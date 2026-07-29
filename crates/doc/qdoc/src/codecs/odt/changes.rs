//! Tracked changes / revision marks ⇄ ODF `text:tracked-changes`. ODF declares every
//! tracked change up front in a `<text:tracked-changes>` block (a direct child of
//! `office:text`) as one `<text:changed-region xml:id="…">` per change, each holding a
//! `<text:insertion>` or `<text:deletion>` with an `<office:change-info>` carrying the
//! author (`<dc:creator>`) and date (`<dc:date>`). The body then references each change
//! by `<text:change-start text:change-id="…"/>` / `<text:change-end text:change-id="…"/>`
//! markers wrapping the affected text (for a point change, a single empty
//! `<text:change text:change-id="…"/>` marker).
//!
//! This maps a tracked change onto the canonical model: an `ins`/`del` mark over the
//! body range carrying the change id, plus the change's author/date/kind in the
//! `changes` container. qdoc *retains* deleted text in the body (struck, not removed),
//! so a deletion round-trips the same way as an insertion — its retained text is wrapped
//! in `text:change-start`/`text:change-end` markers — with a `text:deletion` (rather than
//! `text:insertion`) changed-region recording the kind + author/date.

use crate::snapshot::{ChangeKind, ChangeView};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// One imported changed-region declaration: the ODF `xml:id`, the change kind, and its
/// author/date (from `office:change-info`).
pub(super) struct ImportedChange {
    pub xml_id: String,
    pub kind: ChangeKind,
    pub author: String,
    pub date: String,
}

/// Parse a verbatim `<text:tracked-changes>…</text:tracked-changes>` block into its
/// changed-region declarations (one per tracked change). The body parser later wires
/// each `text:change-start`/`-end`/`text:change` marker (by `xml:id`) onto the matching
/// change via these.
pub(super) fn import_tracked_changes(raw: &str) -> Vec<ImportedChange> {
    let mut out = Vec::new();
    let mut reader = Reader::from_str(raw);
    // Current changed-region: its xml:id + (once seen) kind, plus author/date collected
    // from the nested office:change-info.
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
                        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()));
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

/// Read the `text:change-id` (or `text:id`) attribute of a `text:change-start`/
/// `text:change-end`/`text:change` marker.
pub(super) fn marker_change_id(e: &quick_xml::events::BytesStart) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| matches!(a.key.as_ref(), b"text:change-id" | b"text:id"))
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
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

/// Render the whole `<text:tracked-changes>` block (the change declarations) for a
/// document's changes — emitted at the start of `office:text`. Returns the empty string
/// when there are no changes (so no block is emitted).
pub(super) fn render_tracked_changes(changes: &[ChangeView]) -> String {
    if changes.is_empty() {
        return String::new();
    }
    let mut out = String::from("<text:tracked-changes>");
    for c in changes {
        let info = render_change_info(c);
        let region = match c.kind {
            ChangeKind::Ins => format!("<text:insertion>{info}</text:insertion>"),
            // A deletion's retained text lives in the body (struck), so the changed-region
            // carries only the change-info — the body re-imports the text. An empty
            // `text:p` keeps the element schema-valid.
            ChangeKind::Del => format!("<text:deletion>{info}<text:p/></text:deletion>"),
        };
        out.push_str(&format!(r#"<text:changed-region xml:id="{}" text:id="{}">{region}</text:changed-region>"#, escape_attr(&c.id), escape_attr(&c.id)));
    }
    out.push_str("</text:tracked-changes>");
    out
}

/// The `<office:change-info>` (author + optional date) for a change.
fn render_change_info(c: &ChangeView) -> String {
    let date = if c.date.is_empty() { String::new() } else { format!("<dc:date>{}</dc:date>", escape_text(&c.date)) };
    format!("<office:change-info><dc:creator>{}</dc:creator>{date}</office:change-info>", escape_text(&c.author))
}

/// The `<text:change-start>` marker opening a change's range in the body.
pub(super) fn render_change_start(id: &str) -> String {
    format!(r#"<text:change-start text:change-id="{}"/>"#, escape_attr(id))
}

/// The `<text:change-end>` marker closing a change's range in the body.
pub(super) fn render_change_end(id: &str) -> String {
    format!(r#"<text:change-end text:change-id="{}"/>"#, escape_attr(id))
}
