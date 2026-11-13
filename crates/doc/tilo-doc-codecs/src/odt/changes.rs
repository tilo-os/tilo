//! Tracked changes and ODF's `text:tracked-changes`.
//!
//! ODF declares every change up front as a `<text:changed-region xml:id="…">` holding an insertion
//! or deletion plus its author and date, and wraps the affected range in matching
//! `<text:change-start>`/`<text:change-end>` markers. This maps onto an `ins`/`del` mark over the
//! body range with the change's author, date and kind in the `changes` container; import splices
//! ODF's deleted text back in at the marker under a `del` mark, and export moves it out again.

use crate::xml_escape::{escape_attr, escape_text};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use tilo_doc::document::MarkSpan;
use tilo_doc::schema::{FIELD_PLACEHOLDER, MARK_DEL};
use tilo_doc::snapshot::{ChangeKind, ChangeView, DocSnapshot};

pub(super) struct ImportedChange {
    pub xml_id: String,
    pub kind: ChangeKind,
    pub author: String,
    pub date: String,
    pub deleted: String,
}

pub(super) fn import_tracked_changes(raw: &str) -> Vec<ImportedChange> {
    let mut out = Vec::new();
    let mut reader = Reader::from_str(raw);
    let mut cur_id: Option<String> = None;
    let mut cur_kind: Option<ChangeKind> = None;
    let mut author = String::new();
    let mut date = String::new();
    let mut collecting: Option<bool> = None; // Some(true)=dc:creator, Some(false)=dc:date
    let mut deleted = String::new();
    let mut in_deleted_para = false;
    let mut deleted_paras = 0usize;
    while let Ok(ev) = reader.read_event() {
        let is_start = matches!(ev, Event::Start(_));
        match ev {
            Event::Start(e) | Event::Empty(e)
                if cur_kind == Some(ChangeKind::Del)
                    && matches!(e.name().as_ref(), b"text:p" | b"text:h") =>
            {
                if deleted_paras > 0 {
                    deleted.push('\n');
                }
                deleted_paras = deleted_paras.saturating_add(1);
                in_deleted_para = is_start;
            }
            Event::Empty(e) if in_deleted_para => match e.name().as_ref() {
                b"text:tab" => deleted.push('\t'),
                b"text:line-break" => deleted.push('\n'),
                b"text:s" => {
                    let n = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"text:c")
                        .and_then(|a| crate::unescape_attr(&a))
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(1)
                        .min(256);
                    deleted.push_str(&" ".repeat(n));
                }
                _ => {}
            },
            Event::Start(e) => match e.name().as_ref() {
                b"text:changed-region" => {
                    cur_id = e
                        .attributes()
                        .flatten()
                        .find(|a| matches!(a.key.as_ref(), b"xml:id" | b"text:id"))
                        .and_then(|a| crate::unescape_attr(&a));
                    cur_kind = None;
                    author.clear();
                    date.clear();
                    deleted.clear();
                    deleted_paras = 0;
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
                } else if in_deleted_para && let Ok(txt) = t.xml10_content() {
                    deleted.push_str(&txt);
                }
            }
            Event::GeneralRef(r) => {
                let target = match collecting {
                    Some(true) => &mut author,
                    Some(false) => &mut date,
                    None if in_deleted_para => &mut deleted,
                    None => continue,
                };
                target.extend(crate::resolve_general_ref(&r));
            }
            Event::End(e) => match e.name().as_ref() {
                b"dc:creator" | b"dc:date" => collecting = None,
                b"text:p" | b"text:h" => in_deleted_para = false,
                b"text:changed-region" => {
                    if let Some(id) = cur_id.take() {
                        out.push(ImportedChange {
                            xml_id: id,
                            kind: cur_kind.take().unwrap_or(ChangeKind::Ins),
                            author: std::mem::take(&mut author),
                            date: std::mem::take(&mut date),
                            deleted: std::mem::take(&mut deleted),
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

pub(super) fn marker_change_id(e: &quick_xml::events::BytesStart) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| matches!(a.key.as_ref(), b"text:change-id" | b"text:id"))
        .and_then(|a| crate::unescape_attr(&a))
}

/// Render the `<text:tracked-changes>` block for a document's changes; empty string when
/// there are no changes. A deletion's region holds its deleted text (`deleted`, by change id).
pub(super) fn render_tracked_changes(
    changes: &[ChangeView],
    deleted: &BTreeMap<String, String>,
) -> String {
    if changes.is_empty() {
        return String::new();
    }
    let mut out = String::from("<text:tracked-changes>");
    for c in changes {
        let info = render_change_info(c);
        let region = match c.kind {
            ChangeKind::Ins => format!("<text:insertion>{info}</text:insertion>"),
            ChangeKind::Del => {
                let mut paras = String::new();
                for line in deleted.get(&c.id).map_or("", String::as_str).split('\n') {
                    let _ = write!(paras, "<text:p>{}</text:p>", escape_text(line));
                }
                format!("<text:deletion>{info}{paras}</text:deletion>")
            }
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

pub(super) fn split_tracked_changes(snap: &DocSnapshot) -> (Cow<'_, DocSnapshot>, String) {
    match move_deletions_out(snap) {
        Some((body, deleted)) => {
            let declared = render_tracked_changes(&snap.changes, &deleted);
            (Cow::Owned(body), declared)
        }
        None => (
            Cow::Borrowed(snap),
            render_tracked_changes(&snap.changes, &BTreeMap::new()),
        ),
    }
}

/// ODF keeps a deletion's text in its changed-region, not in the body: replace each text range
/// under a `del` mark by one placeholder carrying the point `text:change` marker (an inline
/// atom, re-emitted verbatim), and collect the removed text per change id. `None` when the
/// document has no deletions, so the common case copies nothing.
pub(super) fn move_deletions_out(
    snap: &DocSnapshot,
) -> Option<(DocSnapshot, BTreeMap<String, String>)> {
    let has_deletions = snap
        .blocks
        .iter()
        .any(|b| b.marks.iter().any(|m| m.name == MARK_DEL));
    if !has_deletions {
        return None;
    }
    let mut deleted: BTreeMap<String, String> = BTreeMap::new();
    let mut out = snap.clone();
    for b in &mut out.blocks {
        let mut ranges: Vec<(usize, usize, String)> = b
            .marks
            .iter()
            .filter(|m| m.name == MARK_DEL && m.end > m.start)
            .filter_map(|m| m.value.as_str().map(|id| (m.start, m.end, id.to_string())))
            .collect();
        if ranges.is_empty() {
            continue;
        }
        ranges.sort();
        let chars: Vec<char> = b.text.chars().collect();
        let mut text = String::new();
        let mut at = 0usize;
        let mut removed: Vec<(usize, usize, bool)> = Vec::new();
        let mut atoms: Vec<(usize, String)> = Vec::new();
        for (start, end, id) in ranges {
            let start = start.max(at).min(chars.len());
            let end = end.min(chars.len());
            if end <= start {
                continue;
            }
            text.extend(chars.get(at..start).unwrap_or_default());
            let piece: String = chars.get(start..end).unwrap_or_default().iter().collect();
            let first = !deleted.contains_key(&id);
            deleted.entry(id.clone()).or_default().push_str(&piece);
            let new_at = text.chars().count();
            if first {
                text.push(FIELD_PLACEHOLDER);
                atoms.push((new_at, render_change_point(&id)));
            }
            removed.push((start, end, first));
            at = end;
        }
        text.extend(chars.get(at..).unwrap_or_default());
        let shift = |p: usize| -> usize {
            let mut lost = 0usize;
            for (s, e, placeholder) in &removed {
                if p >= *e {
                    lost = lost
                        .saturating_add(e.saturating_sub(*s))
                        .saturating_sub(usize::from(*placeholder));
                } else if p > *s {
                    return s.saturating_sub(lost);
                } else {
                    break;
                }
            }
            p.saturating_sub(lost)
        };
        let mut marks: Vec<MarkSpan> = b
            .marks
            .iter()
            .filter(|m| m.name != MARK_DEL)
            .map(|m| MarkSpan {
                start: shift(m.start),
                end: shift(m.end),
                ..m.clone()
            })
            .filter(|m| m.end > m.start)
            .collect();
        for (at, xml) in atoms {
            marks.push(MarkSpan {
                name: crate::preserve::MARK_ODT_XML.to_string(),
                start: at,
                end: at.saturating_add(1),
                value: tilo_doc::document::MarkValue::Str(xml),
            });
        }
        b.text = text;
        b.marks = marks;
    }
    Some((out, deleted))
}

fn render_change_point(id: &str) -> String {
    format!(r#"<text:change text:change-id="{}"/>"#, escape_attr(id))
}

pub(super) fn render_change_start(id: &str) -> String {
    format!(
        r#"<text:change-start text:change-id="{}"/>"#,
        escape_attr(id)
    )
}

pub(super) fn render_change_end(id: &str) -> String {
    format!(r#"<text:change-end text:change-id="{}"/>"#, escape_attr(id))
}
