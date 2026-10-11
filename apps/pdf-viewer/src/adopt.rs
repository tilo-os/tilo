//! Markup that was already in the PDF, taken over as edit records.
//!
//! The first time a document's record meets its source bytes, the reader
//! turns the file's own highlights, underlines, strike-outs, notes (with
//! their reply threads), free text and ink into ordinary annotation and
//! comment records, and its form values into form-value records. From then on
//! they are edited, versioned and shared like any other markup, and the
//! reader draws them from the records (the page image leaves them out).
//!
//! Each taken-over annotation remembers where it came from — page, index and
//! `/NM` in the source bytes — and a fingerprint of what its thread said then
//! ([`thread_print`]). On export an unchanged thread keeps its original PDF
//! objects byte for byte; a changed one replaces them; a deleted one removes
//! them. So an export never shows a piece of markup twice.
//!
//! Imported records get ids derived from the source hash and their address
//! in it, so two peers that import the same bytes concurrently produce the
//! same records, which merge instead of duplicating.
//!
//! A saved export is written to a new file and renamed into place, so it
//! arrives as a new file with a record of its own; opening it takes over the
//! markup it now carries — once, because an annotation whose `/NM` is
//! already a record of the opening record is recognised as its own export.

use std::collections::{HashMap, HashSet};
use ui::tilo_resource;

use tilo_pdf::{AnnotationInfo, AnnotationKind, FieldKind, FormField, PageGeometry};

use crate::model::{
    AnnoKind, Annotation, Comment, FormEntry, GeomSpace, MarkupStyle, PtRect, Reply, Swatch,
};

/// The most annotations one import takes over; a file beyond it keeps the
/// rest as page content.
pub const MAX_IMPORTED_ANNOTATIONS: usize = 5_000;

/// One source page's existing markup, as the engine reads it.
#[derive(Clone, Debug)]
pub struct ExistingPage {
    /// 0-based page of the source bytes.
    pub page: usize,
    pub geometry: PageGeometry,
    pub annotations: Vec<AnnotationInfo>,
}

/// Everything already in a document that records can take over.
#[derive(Clone, Debug, Default)]
pub struct Existing {
    pub pages: Vec<ExistingPage>,
    pub form_fields: Vec<FormField>,
}

/// Where a taken-over annotation came from.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ui::tilo_resource::JsonSchema,
)]
pub struct AdoptedEntry {
    /// The record it became: an annotation id, or a reply id for a reply.
    pub id: String,
    /// 0-based page of the source bytes.
    pub page: usize,
    /// Index in that page's annotations.
    pub index: usize,
    /// `/NM`, possibly empty.
    pub name: String,
    /// The PDF subtype ([`kind_key`]).
    pub kind: String,
    /// For an annotation: the fingerprint of its thread when imported. Empty
    /// for a reply, which follows its parent.
    pub print: String,
    /// For a reply: the annotation it belongs to.
    pub parent: Option<String>,
}

impl AdoptedEntry {
    /// The engine's address for the original.
    pub fn target(&self) -> Option<tilo_pdf::AnnotationRef> {
        Some(tilo_pdf::AnnotationRef {
            page: self.page,
            index: self.index,
            name: self.name.clone(),
            kind: parse_kind(&self.kind)?,
        })
    }
}

/// What an import adds to the record.
#[derive(Clone, Debug, Default)]
pub struct ImportPlan {
    /// Annotations in user space, each with the page occurrence it is on.
    pub annotations: Vec<(Annotation, String)>,
    pub comments: Vec<Comment>,
    pub form_values: Vec<(String, FormEntry)>,
    pub adopted: Vec<AdoptedEntry>,
}

/// A stable key for an annotation subtype.
pub fn kind_key(kind: &AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Text => "text",
        AnnotationKind::FreeText => "freetext",
        AnnotationKind::Highlight => "highlight",
        AnnotationKind::Underline => "underline",
        AnnotationKind::Squiggly => "squiggly",
        AnnotationKind::StrikeOut => "strikeout",
        AnnotationKind::Ink => "ink",
        AnnotationKind::Popup => "popup",
        _ => "other",
    }
}

fn parse_kind(key: &str) -> Option<AnnotationKind> {
    Some(match key {
        "text" => AnnotationKind::Text,
        "freetext" => AnnotationKind::FreeText,
        "highlight" => AnnotationKind::Highlight,
        "underline" => AnnotationKind::Underline,
        "squiggly" => AnnotationKind::Squiggly,
        "strikeout" => AnnotationKind::StrikeOut,
        "ink" => AnnotationKind::Ink,
        "popup" => AnnotationKind::Popup,
        _ => return None,
    })
}

/// The record id an imported annotation gets: stable for the same bytes
/// and address, whoever imports them.
pub fn imported_id(source_hash: &str, page: usize, index: usize) -> String {
    let short = source_hash.get(..16).unwrap_or(source_hash);
    format!("pdf-{short}-{page}-{index}")
}

fn to_user_rect(geometry: &PageGeometry, r: &tilo_pdf::Rect) -> PtRect {
    let u = geometry.rect_to_user(*r);
    PtRect::new(
        f64::from(u.x0),
        f64::from(u.y0),
        f64::from(u.width()),
        f64::from(u.height()),
    )
}

fn to_user_point(geometry: &PageGeometry, p: tilo_pdf::Point) -> (f64, f64) {
    let u = geometry.to_user(p);
    (f64::from(u.x), f64::from(u.y))
}

/// Unix milliseconds for a PDF date (`D:YYYYMMDDHHmmSS…`), when it parses.
pub fn parse_pdf_date(date: &str) -> Option<i64> {
    let digits: String = date
        .strip_prefix("D:")
        .unwrap_or(date)
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let field = |from: usize, len: usize, default: i64| -> i64 {
        digits
            .get(from..from + len)
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    };
    if digits.len() < 4 {
        return None;
    }
    let (year, month, day) = (field(0, 4, 1970), field(4, 2, 1), field(6, 2, 1));
    let (hour, minute, second) = (field(8, 2, 0), field(10, 2, 0), field(12, 2, 0));
    let days =
        ui::date::days_from_civil(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?);
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000)
}

fn color_of(info: &AnnotationInfo, fallback: Swatch) -> Swatch {
    info.color
        .map_or(fallback, |c| Swatch::from_rgb(c.r, c.g, c.b))
}

/// Turn one page's existing annotation into a record annotation (user
/// space), or `None` for kinds and states that stay page content.
fn annotation_of(
    source_hash: &str,
    page: &ExistingPage,
    info: &AnnotationInfo,
) -> Option<Annotation> {
    let g = &page.geometry;
    let kind = match info.kind {
        AnnotationKind::Highlight
        | AnnotationKind::Underline
        | AnnotationKind::StrikeOut
        | AnnotationKind::Squiggly => {
            let style = match info.kind {
                AnnotationKind::Underline => MarkupStyle::Underline,
                AnnotationKind::StrikeOut => MarkupStyle::StrikeOut,
                AnnotationKind::Squiggly => MarkupStyle::Squiggly,
                _ => MarkupStyle::Highlight,
            };
            let quads: Vec<PtRect> = if info.quads.is_empty() {
                vec![to_user_rect(g, &info.rect)]
            } else {
                info.quads.iter().map(|q| to_user_rect(g, q)).collect()
            };
            AnnoKind::Markup { style, quads }
        }
        AnnotationKind::Ink => {
            let strokes: Vec<Vec<(f64, f64)>> = info
                .ink
                .iter()
                .map(|s| s.iter().map(|p| to_user_point(g, *p)).collect::<Vec<_>>())
                .filter(|s: &Vec<(f64, f64)>| !s.is_empty())
                .collect();
            if strokes.is_empty() {
                return None;
            }
            AnnoKind::Ink { strokes }
        }
        AnnotationKind::Text => {
            let r = to_user_rect(g, &info.rect);
            // The note's anchor is its top-left corner in user space.
            AnnoKind::Note {
                x: r.x,
                y: r.y + r.h,
            }
        }
        AnnotationKind::FreeText => AnnoKind::FreeText {
            rect: to_user_rect(g, &info.rect),
            text: info.contents.clone(),
            size: info
                .font_size
                .map_or(crate::model::FREE_TEXT_SIZE, f64::from),
        },
        _ => return None,
    };
    let color = match info.kind {
        AnnotationKind::FreeText => info
            .text_color
            .or(info.color)
            .map_or(Swatch::Custom(0), |c| Swatch::from_rgb(c.r, c.g, c.b)),
        _ => color_of(info, Swatch::Yellow),
    };
    Some(Annotation {
        id: imported_id(source_hash, page.page, info.index),
        page: page.page + 1,
        kind,
        color,
        author: info.author.clone(),
        chars: None,
        created_ms: parse_pdf_date(&info.modified)
            .or_else(|| parse_pdf_date(&info.created))
            .unwrap_or(0),
        space: GeomSpace::User,
    })
}

/// Plan the import of `existing` (read from the bytes hashed `source_hash`)
/// into a record that already holds `known_ids` (annotation and reply ids).
/// Annotations whose `/NM` is a known record id are this record's own
/// earlier export and are not imported again.
#[allow(
    clippy::too_many_lines,
    reason = "one pass over each page: annotations first, then their replies"
)]
pub fn plan_import<S: std::hash::BuildHasher>(
    source_hash: &str,
    existing: &Existing,
    known_ids: &HashSet<String, S>,
) -> ImportPlan {
    let mut plan = ImportPlan::default();
    let mut budget = MAX_IMPORTED_ANNOTATIONS;
    for page in &existing.pages {
        let page_id = format!("source-{}", page.page + 1);
        let by_index: HashMap<usize, &AnnotationInfo> =
            page.annotations.iter().map(|a| (a.index, a)).collect();
        let own_export = |a: &AnnotationInfo| !a.name.is_empty() && known_ids.contains(&a.name);
        // Parents first, so replies find their thread.
        let mut threads: HashMap<usize, usize> = HashMap::new(); // annot index -> comment index
        let mut annotation_ids: HashMap<usize, String> = HashMap::new();
        let is_reply = |a: &AnnotationInfo| {
            a.kind == AnnotationKind::Text
                && a.reply_type == tilo_pdf::ReplyType::Reply
                && a.in_reply_to.is_some_and(|p| {
                    by_index.get(&p).is_some_and(|parent| {
                        parent.kind.is_reader_markup()
                            && parent.kind != AnnotationKind::Popup
                            && !parent.is_hidden()
                    })
                })
        };
        for info in &page.annotations {
            if budget == 0 {
                break;
            }
            if info.is_hidden() || own_export(info) || is_reply(info) {
                continue;
            }
            let Some(annotation) = annotation_of(source_hash, page, info) else {
                continue;
            };
            budget -= 1;
            let id = annotation.id.clone();
            annotation_ids.insert(info.index, id.clone());
            let wants_thread = matches!(annotation.kind, AnnoKind::Note { .. })
                || (!info.contents.trim().is_empty()
                    && !matches!(annotation.kind, AnnoKind::FreeText { .. }));
            if wants_thread {
                threads.insert(info.index, plan.comments.len());
                plan.comments.push(Comment {
                    id: format!("{id}-c"),
                    anno_id: Some(id.clone()),
                    page: annotation.page,
                    author: info.author.clone(),
                    body: info.contents.clone(),
                    created: annotation.created_ms,
                    resolved: false,
                    replies: Vec::new(),
                    blocks: Vec::new(),
                });
            }
            plan.adopted.push(AdoptedEntry {
                id,
                page: page.page,
                index: info.index,
                name: info.name.clone(),
                kind: kind_key(&info.kind).to_string(),
                print: String::new(),
                parent: None,
            });
            plan.annotations.push((annotation, page_id.clone()));
        }
        // Replies, in page order, onto their parent's thread (a reply to a
        // reply joins the same thread).
        for info in &page.annotations {
            if !is_reply(info) || info.is_hidden() || own_export(info) {
                continue;
            }
            let mut root = info.in_reply_to;
            let mut hops = 0;
            while let Some(r) = root
                && !annotation_ids.contains_key(&r)
                && hops < 32
            {
                root = by_index.get(&r).and_then(|a| a.in_reply_to);
                hops += 1;
            }
            let Some(root) = root else { continue };
            let Some(parent_id) = annotation_ids.get(&root).cloned() else {
                continue;
            };
            let comment = if let Some(&c) = threads.get(&root) {
                c
            } else {
                let parent_page = page.page + 1;
                threads.insert(root, plan.comments.len());
                plan.comments.push(Comment {
                    id: format!("{parent_id}-c"),
                    anno_id: Some(parent_id.clone()),
                    page: parent_page,
                    author: by_index
                        .get(&root)
                        .map(|a| a.author.clone())
                        .unwrap_or_default(),
                    body: String::new(),
                    created: 0,
                    resolved: false,
                    replies: Vec::new(),
                    blocks: Vec::new(),
                });
                plan.comments.len() - 1
            };
            let reply_id = imported_id(source_hash, page.page, info.index);
            if let Some(c) = plan.comments.get_mut(comment) {
                c.replies.push(Reply {
                    id: reply_id.clone(),
                    author: info.author.clone(),
                    body: info.contents.clone(),
                    blocks: Vec::new(),
                });
            }
            plan.adopted.push(AdoptedEntry {
                id: reply_id,
                page: page.page,
                index: info.index,
                name: info.name.clone(),
                kind: kind_key(&info.kind).to_string(),
                print: String::new(),
                parent: Some(parent_id),
            });
        }
    }
    // Fingerprint every adopted thread as the record now says it.
    let comments_by_anno: HashMap<&str, &Comment> = plan
        .comments
        .iter()
        .filter_map(|c| Some((c.anno_id.as_deref()?, c)))
        .collect();
    let prints: HashMap<String, String> = plan
        .annotations
        .iter()
        .map(|(a, _)| {
            (
                a.id.clone(),
                thread_print(a, comments_by_anno.get(a.id.as_str()).copied()),
            )
        })
        .collect();
    for entry in &mut plan.adopted {
        if entry.parent.is_none() {
            entry.print = prints.get(&entry.id).cloned().unwrap_or_default();
        }
    }
    plan.form_values = form_values_of(&existing.form_fields);
    plan
}

/// The values a document's own form holds, as records.
pub fn form_values_of(fields: &[FormField]) -> Vec<(String, FormEntry)> {
    let mut out = Vec::new();
    for field in fields {
        if field.name.is_empty() || field.read_only {
            continue;
        }
        let entry = match field.kind {
            FieldKind::Text if !field.value.is_empty() => FormEntry::Text(field.value.clone()),
            FieldKind::ComboBox | FieldKind::ListBox if !field.value.is_empty() => {
                FormEntry::Choice(field.value.clone())
            }
            FieldKind::CheckBox if field.widgets.iter().any(|w| w.checked) => FormEntry::Checked {
                on: true,
                export: None,
            },
            FieldKind::RadioButton => match field.widgets.iter().find(|w| w.checked) {
                Some(w) => FormEntry::Checked {
                    on: true,
                    export: Some(w.export_value.clone()),
                },
                None => continue,
            },
            _ => continue,
        };
        out.push((field.name.clone(), entry));
    }
    out
}

fn push_num(out: &mut String, v: f64) {
    use std::fmt::Write as _;
    // Two decimals: what records keep, and immune to float noise.
    let _ = write!(out, "{:.2},", (v * 100.0).round() / 100.0);
}

/// A fingerprint of everything an exported thread says: the annotation
/// (user space) and its comment with replies. Equal prints mean the export
/// would write what the original already says.
pub fn thread_print(a: &Annotation, thread: Option<&Comment>) -> String {
    let mut s = String::new();
    s.push_str(a.kind.type_key());
    s.push('|');
    match &a.kind {
        AnnoKind::Markup { quads, .. } => {
            for q in quads {
                for v in [q.x, q.y, q.w, q.h] {
                    push_num(&mut s, v);
                }
            }
        }
        AnnoKind::Ink { strokes } => {
            for stroke in strokes {
                for (x, y) in stroke {
                    push_num(&mut s, *x);
                    push_num(&mut s, *y);
                }
                s.push(';');
            }
        }
        AnnoKind::Note { x, y } => {
            push_num(&mut s, *x);
            push_num(&mut s, *y);
        }
        AnnoKind::FreeText { rect, text, size } => {
            for v in [rect.x, rect.y, rect.w, rect.h, *size] {
                push_num(&mut s, v);
            }
            s.push_str(text);
        }
    }
    s.push('|');
    s.push_str(&a.color.key());
    s.push('|');
    s.push_str(&a.author);
    if let Some(c) = thread {
        s.push('|');
        s.push_str(&c.body);
        for r in &c.replies {
            s.push('|');
            s.push_str(&r.id);
            s.push('\u{1f}');
            s.push_str(&r.author);
            s.push('\u{1f}');
            s.push_str(&r.body);
        }
    }
    tilo_content::hash_bytes(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_dates_and_ids_are_deterministic() {
        assert_eq!(parse_pdf_date("D:19700101000000Z"), Some(0));
        assert_eq!(parse_pdf_date("D:20260924120000Z"), Some(1_790_251_200_000));
        assert_eq!(parse_pdf_date(""), None);

        let hash = "ab".repeat(32);
        assert_eq!(imported_id(&hash, 1, 3), imported_id(&hash, 1, 3));
        assert_ne!(imported_id(&hash, 1, 3), imported_id(&hash, 1, 4));
    }
}
