//! The built-in Markdown codec: the modeled rich-text subset (headings, paragraphs, lists, marks,
//! tables, footnotes, code blocks) mapped onto the canonical document, and back.
//!
//! Export normalizes whitespace, and unmodeled constructs are not preserved losslessly.

use crate::format::{CodecError, FormatProvider};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use tilo_content::{ContentInputs, ContentSource};
use tilo_doc::document::MarkSpan;
use tilo_doc::export::{CellGrid, ExportBlock};
use tilo_doc::model::{DocMut, DocRef};
use tilo_doc::schema::{
    MARK_NOTE_REF, PROP_AUTHOR, PROP_KEYWORDS, PROP_MODIFIED, PROP_SUBJECT, PROP_TITLE,
};
use tilo_doc::snapshot::{BlockView, NoteView};

pub struct MarkdownCodec;

impl FormatProvider for MarkdownCodec {
    fn import(
        &self,
        bytes: &[u8],
        doc: &mut DocMut,
        _inputs: &mut ContentInputs,
    ) -> Result<(), CodecError> {
        doc.bulk(|doc| {
            let src = std::str::from_utf8(bytes).map_err(|e| CodecError::Parse(e.to_string()))?;
            let body = apply_front_matter(src, doc);
            import_md(body, doc);
            Ok(())
        })
    }

    fn export(&self, doc: DocRef, _content: &dyn ContentSource) -> Result<Vec<u8>, CodecError> {
        let snapshot = doc.materialize();
        Ok(export_views(
            &snapshot.properties,
            &snapshot.blocks,
            &snapshot.notes,
        ))
    }
}

const FRONT_MATTER_MAP: [(&str, &str); 6] = [
    ("title", PROP_TITLE),
    ("author", PROP_AUTHOR),
    ("date", PROP_MODIFIED),
    ("subject", PROP_SUBJECT),
    ("keywords", PROP_KEYWORDS),
    ("lang", "lang"),
];

fn front_matter_prop(key: &str) -> Option<&'static str> {
    let k = key.trim().to_ascii_lowercase();
    let k = match k.as_str() {
        "language" => "lang",
        other => other,
    };
    FRONT_MATTER_MAP
        .iter()
        .find(|(fk, _)| *fk == k)
        .map(|(_, p)| *p)
}

/// If `src` opens with a `---` front-matter block, parses its `key: value` scalar
/// lines into document properties and returns the remaining body (no YAML dependency,
/// bare scalars only); a malformed/unterminated block is returned verbatim as body.
fn apply_front_matter<'a>(src: &'a str, doc: &mut DocMut) -> &'a str {
    let Some(rest) = src
        .strip_prefix("---\n")
        .or_else(|| src.strip_prefix("---\r\n"))
    else {
        return src;
    };
    let mut idx = 0usize;
    let mut body_start: Option<usize> = None;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" || trimmed == "..." {
            body_start = Some(idx.saturating_add(line.len()));
            break;
        }
        idx = idx.saturating_add(line.len());
    }
    let Some(body_start) = body_start else {
        return src;
    };
    let (Some(yaml), Some(body)) = (rest.get(..idx), rest.get(body_start..)) else {
        return src;
    };

    for line in yaml.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let Some(prop) = front_matter_prop(key) else {
            continue;
        };
        let val = strip_scalar_quotes(val.trim());
        if !val.is_empty() {
            doc.set_doc_prop(prop, &val);
        }
    }
    body
}

fn strip_scalar_quotes(v: &str) -> String {
    if let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return inner.replace("\\\"", "\"");
    }
    if let Some(inner) = v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return inner.to_string();
    }
    v.to_string()
}

fn render_front_matter(props: &std::collections::BTreeMap<String, String>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (fm_key, prop) in FRONT_MATTER_MAP {
        if let Some(v) = props.get(prop) {
            if v.is_empty() {
                continue;
            }
            let needs_quote = v.contains(':') || v.starts_with(['"', '\'', ' ', '#']);
            if needs_quote {
                lines.push(format!("{fm_key}: \"{}\"", v.replace('"', "\\\"")));
            } else {
                lines.push(format!("{fm_key}: {v}"));
            }
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("---\n{}\n---\n\n", lines.join("\n"))
}

fn start_block(
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    kind: &str,
) -> String {
    let id = doc.insert_block(prev.as_deref(), kind);
    *prev = Some(id.clone());
    *cur = Some(id.clone());
    *cur_len = 0;
    id
}

struct PendingMark {
    block: String,
    start: usize,
    end: usize,
    name: &'static str,
    value: Option<String>,
}

/// A styling mark parsed from `<span style=…>`, e.g. `("color","#ff0000")`; names match the editor's run marks.
fn marks_from_span_style(style: &str) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for decl in style.split(';') {
        let Some((prop, val)) = decl.split_once(':') else {
            continue;
        };
        let prop = prop.trim().to_ascii_lowercase();
        let val = val.trim();
        match prop.as_str() {
            "color" => {
                out.push(("color", normalize_css_color(val)));
            }
            "background" | "background-color" => {
                out.push(("highlight", normalize_css_color(val)));
            }
            "font-size" => {
                let num = val
                    .trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%')
                    .trim();
                if let Ok(f) = num.parse::<f64>() {
                    // Floats have no checked int conversion; clamp to a range no real
                    // CSS font-size approaches before the cast, rather than trust the input.
                    let clamped = f.round().clamp(-1_000_000.0, 1_000_000.0);
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "clamped to ±1_000_000 above, far inside i64 range"
                    )]
                    let n = clamped as i64;
                    out.push(("fontSize", n.to_string()));
                }
            }
            "font-family" => {
                let g = crate::generic_font_family(val);
                out.push(("fontFamily", g.to_string()));
            }
            _ => {}
        }
    }
    out
}

fn normalize_css_color(v: &str) -> String {
    let t = v.trim();
    if let Some(hex) = t.strip_prefix('#')
        && hex.len() == 6
        && hex.chars().all(|c| c.is_ascii_hexdigit())
    {
        return format!("#{}", hex.to_ascii_lowercase());
    }
    match t.to_ascii_lowercase().as_str() {
        "red" => "#ff0000".into(),
        "green" => "#008000".into(),
        "blue" => "#0000ff".into(),
        "black" => "#000000".into(),
        "white" => "#ffffff".into(),
        "yellow" => "#ffff00".into(),
        _ => t.to_string(),
    }
}

fn span_style_attr(tag: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let idx = lower.find("style")?;
    // `to_ascii_lowercase` only rewrites ASCII bytes in place, so byte offsets in `lower`
    // are valid offsets into `tag` too.
    let rest = tag.get(idx.checked_add("style".len())?..)?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let after = rest.get(1..)?;
    let end = after.find(quote)?;
    Some(after.get(..end)?.to_string())
}

/// Per-document parser state threaded through the pulldown-cmark event loop; bundled into one
/// struct so the loop can be split into named helpers below instead of one 300-line function.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per state the event walker carries: a list item, a table cell, a quote, \
              and the line break an alert's marker took"
)]
#[derive(Default)]
struct ImportState {
    prev: Option<String>,
    cur: Option<String>,
    cur_len: usize,
    in_item: bool,
    in_cell: bool,
    ordered_stack: Vec<bool>,
    mark_stack: Vec<(&'static str, usize)>,
    link_stack: Vec<(String, usize)>,
    span_stack: Vec<(Vec<(&'static str, String)>, usize)>,
    wrap_stack: Vec<(&'static str, usize)>,
    pending_marks: Vec<PendingMark>,
    table_id: Option<String>,
    trow: usize,
    tcol: usize,
    table_seq: usize,
    note_ids: BTreeMap<String, String>,
    ref_sites: Vec<(String, usize, String)>,
    footnote_bodies: BTreeMap<String, Vec<String>>,
    def_label: Option<String>,
    def_para: Option<String>,
    /// The addresses of the links open inside a footnote definition.
    def_links: Vec<String>,
    image: Option<ImageIn>,
    in_quote: bool,
    quote_block: Option<String>,
    /// The text a quote's first line has said so far, while it may still be an alert's marker:
    /// a reader splits `[!WARNING]` into several text events, so the marker is read as it
    /// arrives.
    quote_prefix: Option<String>,
    /// The space after an alert's marker, which the marker takes with it.
    trim_space: bool,
    /// The line break after an alert's marker, which the marker took with it.
    skip_break: bool,
}

/// What a quote's first line has said of an alert's marker so far.
enum AlertRead {
    /// The marker may still be coming.
    Waiting,
    /// The line does not open with a marker.
    NotOne,
    /// It is this alert's; the marker and its following space are `marker` characters from the
    /// block's start, and a space yet to arrive is `trim`med.
    One {
        kind: &'static str,
        marker: usize,
        trim: bool,
    },
}

/// The callouts a quote can be, by the name its `alert` attribute stores: GitHub-flavoured
/// Markdown's alerts.
pub const CALLOUTS: [&str; 5] = ["note", "tip", "important", "warning", "caution"];

/// The callout `name` names, its case aside.
#[must_use]
pub fn callout_named(name: &str) -> Option<&'static str> {
    let name = name.trim();
    CALLOUTS
        .into_iter()
        .find(|callout| callout.eq_ignore_ascii_case(name))
}

const CALLOUT_ATTR: &str = "alert";

fn alert_of(text: &str) -> Option<(&'static str, &str)> {
    let rest = text.strip_prefix("[!")?;
    let (name, said) = rest.split_once(']')?;
    Some((callout_named(name)?, said))
}

struct ImageIn {
    source: String,
    start: usize,
    alt: String,
}

impl ImageIn {
    /// The Markdown the image is written as, as the image was read.
    fn markdown(&self) -> String {
        format!("![{}]({})", self.alt, self.source)
    }
}

impl ImportState {
    fn note_id_for(&mut self, doc: &mut DocMut, label: &str) -> String {
        self.note_ids
            .entry(label.to_string())
            .or_insert_with(|| doc.insert_note(tilo_doc::model::NoteKind::Footnote))
            .clone()
    }

    /// Inside a footnote definition, text is captured into the note body, not the document
    /// body. Returns `true` when the event was consumed by capture mode.
    fn capture_footnote_definition(&mut self, ev: &Event) -> bool {
        if self.def_label.is_none() {
            return false;
        }
        match ev {
            Event::End(TagEnd::FootnoteDefinition) => {
                if let (Some(label), Some(p)) = (self.def_label.take(), self.def_para.take()) {
                    self.footnote_bodies.entry(label).or_default().push(p);
                } else {
                    self.def_label = None;
                    self.def_para = None;
                }
            }
            Event::Start(Tag::Paragraph) => self.def_para = Some(String::new()),
            Event::End(TagEnd::Paragraph) => {
                if let (Some(label), Some(p)) = (self.def_label.clone(), self.def_para.take()) {
                    self.footnote_bodies.entry(label).or_default().push(p);
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some(p) = self.def_para.as_mut() {
                    p.push_str(t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(p) = self.def_para.as_mut() {
                    p.push(' ');
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => self.def_links.push(dest_url.to_string()),
            Event::End(TagEnd::Link) => {
                // A note holds words only: the address a link names is written out after
                // its words, so the source a footnote cites is never lost.
                if let (Some(url), Some(p)) = (self.def_links.pop(), self.def_para.as_mut())
                    && !url.is_empty()
                    && !p.ends_with(url.as_str())
                {
                    p.push_str(" (");
                    p.push_str(&url);
                    p.push(')');
                }
            }
            _ => {}
        }
        true
    }

    fn handle_block_event(&mut self, doc: &mut DocMut, ev: &Event) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                let id = start_block(
                    doc,
                    &mut self.prev,
                    &mut self.cur,
                    &mut self.cur_len,
                    "heading",
                );
                doc.set_attr(&id, "level", *level as i64);
            }
            Event::End(TagEnd::Heading(_) | TagEnd::CodeBlock) => self.cur = None,
            // A quote: its paragraphs are the quote's, which a view draws set apart — and a
            // GitHub alert is a quote whose text opens with its marker, `[!WARNING]`, which the
            // document stores as the quote's `alert` attribute and never as words.
            Event::Start(Tag::BlockQuote(_)) => self.in_quote = true,
            Event::End(TagEnd::BlockQuote(_)) => {
                self.cur = None;
                self.in_quote = false;
            }
            Event::Rule => {
                start_block(
                    doc,
                    &mut self.prev,
                    &mut self.cur,
                    &mut self.cur_len,
                    "divider",
                );
                self.cur = None;
            }
            Event::Start(Tag::Paragraph) => {
                if !self.in_item && !self.in_cell {
                    let id = start_block(
                        doc,
                        &mut self.prev,
                        &mut self.cur,
                        &mut self.cur_len,
                        if self.in_quote { "quote" } else { "paragraph" },
                    );
                    self.quote_block = self.in_quote.then_some(id);
                    self.quote_prefix = self.in_quote.then(String::new);
                    self.trim_space = false;
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if !self.in_item && !self.in_cell {
                    self.cur = None;
                }
                self.quote_block = None;
                self.quote_prefix = None;
            }
            Event::Start(Tag::List(start)) => self.ordered_stack.push(start.is_some()),
            Event::End(TagEnd::List(_)) => {
                self.ordered_stack.pop();
            }
            Event::Start(Tag::Item) => {
                let ordered = *self.ordered_stack.last().unwrap_or(&false);
                let level = self.ordered_stack.len().saturating_sub(1);
                let id = start_block(
                    doc,
                    &mut self.prev,
                    &mut self.cur,
                    &mut self.cur_len,
                    "list_item",
                );
                doc.set_attr(&id, "ordered", ordered);
                if level > 0
                    && let Ok(level) = i64::try_from(level)
                {
                    doc.set_attr(&id, "level", level);
                }
                self.in_item = true;
            }
            Event::End(TagEnd::Item) => {
                self.cur = None;
                self.in_item = false;
            }
            Event::TaskListMarker(checked) => {
                if let Some(b) = self.cur.as_deref() {
                    doc.set_attr(b, "checked", *checked);
                }
            }
            _ => self.handle_table_or_code_event(doc, ev),
        }
    }

    fn handle_table_or_code_event(&mut self, doc: &mut DocMut, ev: &Event) {
        match ev {
            Event::Start(Tag::Table(_)) => {
                self.table_seq = self.table_seq.saturating_add(1);
                self.table_id = Some(format!("t{}", self.table_seq));
                self.trow = 0;
            }
            Event::End(TagEnd::Table) => self.table_id = None,
            Event::Start(Tag::TableHead | Tag::TableRow) => self.tcol = 0,
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                self.trow = self.trow.saturating_add(1);
            }
            Event::Start(Tag::TableCell) => {
                if let Some(tid) = self.table_id.clone() {
                    let id = start_block(
                        doc,
                        &mut self.prev,
                        &mut self.cur,
                        &mut self.cur_len,
                        "cell",
                    );
                    doc.set_attr(&id, "table", tid.as_str());
                    if let (Ok(row), Ok(col)) = (i64::try_from(self.trow), i64::try_from(self.tcol))
                    {
                        doc.set_attr(&id, "row", row);
                        doc.set_attr(&id, "col", col);
                    }
                    self.in_cell = true;
                }
            }
            Event::End(TagEnd::TableCell) => {
                self.cur = None;
                self.in_cell = false;
                self.tcol = self.tcol.saturating_add(1);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let id = start_block(
                    doc,
                    &mut self.prev,
                    &mut self.cur,
                    &mut self.cur_len,
                    "paragraph",
                );
                doc.set_attr(&id, "code", true);
                if let CodeBlockKind::Fenced(lang) = kind
                    && !lang.is_empty()
                {
                    doc.set_attr(&id, "lang", lang.as_ref());
                }
            }
            _ => {}
        }
    }

    /// Toggle-mark and inline-content events: strong/em/strike, links, footnote refs, text,
    /// inline code, breaks, and the `<span>`/`<sup>`/`<sub>` HTML fallback.
    #[expect(
        clippy::too_many_lines,
        reason = "the dispatch over every inline event, one arm each"
    )]
    fn handle_inline_event(&mut self, doc: &mut DocMut, ev: &Event) {
        match ev {
            Event::Start(Tag::Strong) => self.mark_stack.push(("strong", self.cur_len)),
            Event::Start(Tag::Emphasis) => self.mark_stack.push(("em", self.cur_len)),
            Event::Start(Tag::Strikethrough) => self.mark_stack.push(("strike", self.cur_len)),
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                if let (Some((name, start)), Some(b)) = (self.mark_stack.pop(), self.cur.as_ref()) {
                    self.pending_marks.push(PendingMark {
                        block: b.clone(),
                        start,
                        end: self.cur_len,
                        name,
                        value: None,
                    });
                }
            }
            Event::Start(Tag::FootnoteDefinition(label)) => {
                let label = label.to_string();
                let _ = self.note_id_for(doc, &label);
                self.def_label = Some(label);
                self.def_para = None;
            }
            Event::FootnoteReference(label) => {
                if let Some(b) = self.cur.clone() {
                    let note_id = self.note_id_for(doc, label);
                    doc.splice_text(
                        &b,
                        self.cur_len,
                        0,
                        &tilo_doc::schema::FIELD_PLACEHOLDER.to_string(),
                    );
                    self.ref_sites.push((b, self.cur_len, note_id));
                    self.cur_len = self.cur_len.saturating_add(1);
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.link_stack.push((dest_url.to_string(), self.cur_len));
            }
            Event::End(TagEnd::Link) => {
                if let (Some((href, start)), Some(b)) = (self.link_stack.pop(), self.cur.as_ref()) {
                    self.pending_marks.push(PendingMark {
                        block: b.clone(),
                        start,
                        end: self.cur_len,
                        name: "link",
                        value: Some(href),
                    });
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                self.image = Some(ImageIn {
                    source: dest_url.to_string(),
                    start: self.cur_len,
                    alt: String::new(),
                });
            }
            Event::End(TagEnd::Image) => {
                if let (Some(image), Some(b)) = (self.image.take(), self.cur.as_ref()) {
                    let written = image.markdown();
                    doc.splice_text(b, image.start, 0, &written);
                    self.cur_len = image.start.saturating_add(written.chars().count());
                }
            }
            Event::Text(t) => {
                // A text after the marker means the break the marker would have taken is not
                // coming.
                self.skip_break = false;
                if let Some(image) = self.image.as_mut() {
                    image.alt.push_str(t);
                } else if let Some(b) = self.cur.clone() {
                    let text = if std::mem::take(&mut self.trim_space) {
                        t.strip_prefix(' ').unwrap_or_else(|| t.as_ref())
                    } else {
                        t.as_ref()
                    };
                    doc.splice_text(&b, self.cur_len, 0, text);
                    self.cur_len = self.cur_len.saturating_add(text.chars().count());
                    self.read_alert(doc, &b, text);
                }
            }
            Event::Code(t) => {
                if let Some(b) = self.cur.as_ref() {
                    let start = self.cur_len;
                    doc.splice_text(b, self.cur_len, 0, t);
                    self.cur_len = self.cur_len.saturating_add(t.chars().count());
                    self.pending_marks.push(PendingMark {
                        block: b.clone(),
                        start,
                        end: self.cur_len,
                        name: "fontFamily",
                        value: Some("mono".to_string()),
                    });
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                // A break right after an alert's marker belongs to the marker it took.
                if !std::mem::take(&mut self.skip_break) {
                    if let Some(image) = self.image.as_mut() {
                        image.alt.push(' ');
                    } else if let Some(b) = self.cur.as_ref() {
                        doc.splice_text(b, self.cur_len, 0, " ");
                        self.cur_len = self.cur_len.saturating_add(1);
                    }
                }
            }
            Event::InlineHtml(html) | Event::Html(html) => self.handle_inline_html(html),
            _ => {}
        }
    }

    /// A quote that opens with an alert's marker is that alert: the marker is not part of what
    /// it says, and the block keeps which alert it is. A reader hands the marker over in
    /// pieces (`[`, `!WARNING`, `]`), so it is read as each piece arrives.
    fn read_alert(&mut self, doc: &mut DocMut, block: &str, text: &str) {
        if self.quote_block.as_deref() != Some(block) {
            return;
        }
        let read = {
            let Some(prefix) = self.quote_prefix.as_mut() else {
                return;
            };
            prefix.push_str(text);
            match prefix.find(']') {
                None if prefix.chars().count() > 24 => AlertRead::NotOne,
                None => AlertRead::Waiting,
                Some(close) => match alert_of(prefix.get(..=close).unwrap_or_default()) {
                    None => AlertRead::NotOne,
                    Some((kind, _)) => {
                        // The marker and the space after it are the marker's, not the
                        // quote's; a space that has not arrived yet is dropped when it does.
                        let rest = prefix.get(close.saturating_add(1)..).unwrap_or_default();
                        let (space, trim) = match rest.chars().next() {
                            None => (0, true),
                            Some(' ') => (1, false),
                            Some(_) => (0, false),
                        };
                        AlertRead::One {
                            kind,
                            marker: close.saturating_add(1).saturating_add(space),
                            trim,
                        }
                    }
                },
            }
        };
        match read {
            AlertRead::Waiting => {}
            AlertRead::NotOne => self.quote_prefix = None,
            AlertRead::One { kind, marker, trim } => {
                doc.splice_text(block, 0, isize::try_from(marker).unwrap_or(isize::MAX), "");
                doc.set_attr(block, CALLOUT_ATTR, kind);
                self.cur_len = self.cur_len.saturating_sub(marker);
                self.quote_prefix = None;
                self.trim_space = trim;
                if self.cur_len == 0 {
                    self.skip_break = true;
                }
            }
        }
    }

    fn handle_inline_html(&mut self, html: &str) {
        let t = html.trim();
        let lower = t.to_ascii_lowercase();
        if t.eq_ignore_ascii_case("</span>") {
            if let (Some((marks, start)), Some(b)) = (self.span_stack.pop(), self.cur.clone()) {
                for (name, value) in marks {
                    self.pending_marks.push(PendingMark {
                        block: b.clone(),
                        start,
                        end: self.cur_len,
                        name,
                        value: Some(value),
                    });
                }
            }
        } else if lower.starts_with("<span") {
            let marks = span_style_attr(t)
                .map(|s| marks_from_span_style(&s))
                .unwrap_or_default();
            self.span_stack.push((marks, self.cur_len));
        } else if lower == "<sup>" {
            self.wrap_stack.push(("super", self.cur_len));
        } else if lower == "<sub>" {
            self.wrap_stack.push(("sub", self.cur_len));
        } else if lower == "</sup>" || lower == "</sub>" {
            let want = if lower == "</sup>" { "super" } else { "sub" };
            if let Some(pos) = self.wrap_stack.iter().rposition(|(n, _)| *n == want) {
                let (name, start) = self.wrap_stack.remove(pos);
                if let Some(b) = self.cur.as_ref() {
                    self.pending_marks.push(PendingMark {
                        block: b.clone(),
                        start,
                        end: self.cur_len,
                        name,
                        value: None,
                    });
                }
            }
        }
    }

    /// Applies every mark gathered during parsing, then the footnote-reference and
    /// footnote-body wiring; called once after the event loop ends.
    fn finish(self, doc: &mut DocMut) {
        for pm in self.pending_marks {
            match pm.value {
                Some(v) if pm.name == "fontSize" => {
                    if let Ok(n) = v.parse::<i64>() {
                        doc.add_mark(&pm.block, pm.start, pm.end, pm.name, n);
                    }
                }
                Some(v) => doc.add_mark(&pm.block, pm.start, pm.end, pm.name, v.as_str()),
                None => doc.add_mark(&pm.block, pm.start, pm.end, pm.name, true),
            }
        }

        for (block, idx, note_id) in self.ref_sites {
            let end = idx.saturating_add(1);
            doc.add_mark(&block, idx, end, MARK_NOTE_REF, note_id.as_str());
        }

        for (label, paras) in self.footnote_bodies {
            let Some(note_id) = self.note_ids.get(&label) else {
                continue;
            };
            let mut prev_nb: Option<String> = None;
            for para in paras {
                if let Some(nb) = doc.insert_note_block(note_id, prev_nb.as_deref(), "paragraph") {
                    if !para.is_empty() {
                        doc.splice_note_text(note_id, &nb, 0, 0, &para);
                    }
                    prev_nb = Some(nb);
                }
            }
        }
    }
}

fn import_md(src: &str, doc: &mut DocMut) {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_FOOTNOTES);

    let mut state = ImportState::default();
    for ev in Parser::new_ext(src, opts) {
        if state.capture_footnote_definition(&ev) {
            continue;
        }
        state.handle_block_event(doc, &ev);
        state.handle_inline_event(doc, &ev);
    }
    state.finish(doc);
}

/// Export Markdown from complete, already materialized document views.
///
/// The caller supplies properties, body blocks in document order, and notes from
/// the same document revision. This uses the same renderer as [`MarkdownCodec`]
/// without reading or materializing a CRDT. Like the normal Markdown export,
/// headers, footers, comments, and image blob metadata are not represented.
pub fn export_views(
    properties: &BTreeMap<String, String>,
    blocks: &[BlockView],
    notes: &[NoteView],
) -> Vec<u8> {
    let blocks = tilo_doc::export::without_kept_placeholders_in(blocks);
    export_md(properties, &blocks, notes).into_bytes()
}

fn export_md(
    properties: &BTreeMap<String, String>,
    blocks: &[BlockView],
    notes: &[NoteView],
) -> String {
    let mut out = String::new();
    out.push_str(&render_front_matter(properties));
    let body_start = out.len();
    let mut footnotes = Footnotes::new(notes);
    let mut i = 0;
    let mut prev_kind: Option<&str> = None;
    while let Some(b) = blocks.get(i) {
        if let Some(pk) = prev_kind {
            if pk == "list_item" && b.kind == "list_item" {
                out.push('\n');
            } else {
                out.push_str("\n\n");
            }
        }

        match b.kind.as_str() {
            "cell" => {
                let grid = CellGrid::run(blocks, i);
                i = i.saturating_add(grid.cells().len().max(1));
                out.push_str(&render_table(&grid));
                prev_kind = Some("cell");
                continue;
            }
            "heading" => {
                out.push_str(&"#".repeat(usize::from(ExportBlock::new(b).heading_level())));
                out.push(' ');
                out.push_str(&render_inline_notes(
                    &b.text,
                    &b.marks,
                    Some(&mut footnotes),
                ));
            }
            "quote" | "blockquote" => out.push_str(&render_quote(b, &mut footnotes)),
            "divider" => out.push_str("---"),
            "list_item" => {
                let item = ExportBlock::new(b);
                out.push_str(&"  ".repeat(usize::try_from(item.list_level()).unwrap_or(0)));
                out.push_str(if item.ordered() { "1. " } else { "- " });
                if let Some(checked) = b.bool_attr("checked") {
                    out.push_str(if checked { "[x] " } else { "[ ] " });
                }
                out.push_str(&render_inline_notes(
                    &b.text,
                    &b.marks,
                    Some(&mut footnotes),
                ));
            }
            _ => {
                if b.flag("code") {
                    let lang = b.str_attr("lang").unwrap_or("");
                    let lines: Vec<&str> = b.text.split('\n').collect();
                    let fence = fence_for(&lines);
                    out.push_str(&fence);
                    out.push_str(lang);
                    out.push('\n');
                    out.push_str(&b.text);
                    if !b.text.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&fence);
                } else if b.kind == "paragraph"
                    && b.marks.is_empty()
                    && (picture_source(&b.text) || b.flag(crate::rich::TABLE_SOURCE))
                {
                    // A picture, and a table kept as its Markdown source, are the Markdown they
                    // are written as; written raw, a reader of the file shows the picture and
                    // the table, not the markup.
                    out.push_str(&b.text);
                } else {
                    let said = render_inline_notes(&b.text, &b.marks, Some(&mut footnotes));
                    if b.kind == "paragraph" {
                        out.push_str(&guard_paragraph(&said));
                    } else {
                        out.push_str(&said);
                    }
                }
            }
        }
        prev_kind = Some(&b.kind);
        i = i.saturating_add(1);
    }
    out.push('\n');
    let defs = footnotes.render_definitions();
    if !defs.is_empty() {
        // Separate defs from body with a blank line; if body was empty (only front-matter), defs become the whole doc.
        if out.len() > body_start
            && out
                .get(body_start..)
                .is_some_and(|rest| !rest.trim().is_empty())
        {
            out.push('\n');
        }
        out.push_str(&defs);
    }
    out
}

/// The backticks a fenced block of `lines` takes: three, or one more than the longest run a
/// line opens with, since a run of that many backticks would close a shorter fence and end the
/// block early. Markdown closes a fence on a line indented up to three spaces, such as the
/// fence of a list item the code shows.
#[must_use]
pub fn fence_for(lines: &[&str]) -> String {
    let longest = lines
        .iter()
        .map(|line| line.trim_start().chars().take_while(|c| *c == '`').count())
        .filter(|run| *run >= 3)
        .max()
        .unwrap_or(0);
    "`".repeat(longest.saturating_add(1).max(3))
}

/// A quote as Markdown: each of its lines set apart (`> …`), and an alert of one opening with
/// its marker, which reads back as the same alert.
fn render_quote(block: &BlockView, footnotes: &mut Footnotes<'_>) -> String {
    let mut out = String::new();
    let alert = block
        .str_attr(CALLOUT_ATTR)
        .and_then(callout_named)
        .map(str::to_ascii_uppercase);
    if let Some(alert) = alert {
        let _ = writeln!(out, "> [!{alert}]");
    }
    let said = render_inline_notes(&block.text, &block.marks, Some(footnotes));
    for (index, line) in said.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str("> ");
        out.push_str(line);
    }
    out
}

fn escape_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Renders a run of `cell` blocks (one table) as a GitHub pipe table (row 0 = header, then a delimiter row).
fn render_table(grid: &CellGrid<'_>) -> String {
    let (nrows, ncols) = (grid.rows(), grid.cols());
    let mut out = String::new();
    for r in 0..nrows {
        out.push('|');
        for c in 0..ncols {
            let txt = grid
                .origin(r, c)
                .map(|cl| escape_cell(&render_inline(&cl.text, &cl.marks)))
                .unwrap_or_default();
            out.push(' ');
            out.push_str(&txt);
            out.push_str(" |");
        }
        out.push('\n');
        if r == 0 {
            out.push('|');
            for _ in 0..ncols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// A paragraph that is only a picture: the Markdown a view draws as an image, `![alt](source)`,
/// and nothing beside it.
fn picture_source(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("![") else {
        return false;
    };
    let Some((alt, target)) = rest.split_once("](") else {
        return false;
    };
    let Some(target) = target.strip_suffix(')') else {
        return false;
    };
    !alt.contains(['\n', '[']) && !target.is_empty() && !target.contains(['\n', ')'])
}

/// A paragraph's text must not read as a block marker: a line that opens like a heading, a
/// quote, a bullet or a numbered item is escaped where the marker starts.
#[must_use]
pub fn guard_paragraph(rendered: &str) -> String {
    let chars: Vec<char> = rendered.chars().collect();
    let first = chars.first().copied();
    let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    let numbered = digits > 0 && matches!(chars.get(digits), Some('.' | ')'));
    match first {
        Some('#' | '>' | '-' | '+') => format!("\\{rendered}"),
        _ if numbered => {
            let head: String = chars.iter().take(digits).collect();
            let tail: String = chars.iter().skip(digits).collect();
            format!("{head}\\{tail}")
        }
        _ => rendered.to_owned(),
    }
}

/// Export-time footnote registry: assigns each referenced note a sequential 1-based
/// number in first-encounter order, so refs render `[^1]`, `[^2]`, … with defs appended at the end.
struct Footnotes<'a> {
    index: std::collections::BTreeMap<String, &'a NoteView>,
    numbers: std::collections::BTreeMap<String, usize>,
    order: Vec<String>,
}

impl<'a> Footnotes<'a> {
    fn new(notes: &'a [NoteView]) -> Self {
        Footnotes {
            index: notes.iter().map(|n| (n.id.clone(), n)).collect(),
            numbers: std::collections::BTreeMap::new(),
            order: Vec::new(),
        }
    }

    /// 1-based number for a referenced note, assigning the next one on first encounter.
    fn number(&mut self, note_id: &str) -> usize {
        if let Some(n) = self.numbers.get(note_id) {
            return *n;
        }
        let n = self.order.len().saturating_add(1);
        self.numbers.insert(note_id.to_string(), n);
        self.order.push(note_id.to_string());
        n
    }

    /// Renders `[^N]: body` for every referenced note in assignment order; empty if unused.
    fn render_definitions(&self) -> String {
        let mut out = String::new();
        for id in &self.order {
            let Some(n) = self.numbers.get(id) else {
                continue;
            };
            let body = self
                .index
                .get(id)
                .map(|note| {
                    note.blocks
                        .iter()
                        .map(|b| b.text.replace('\n', " "))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let _ = writeln!(out, "[^{n}]: {body}");
        }
        out
    }
}

fn render_inline(text: &str, marks: &[MarkSpan]) -> String {
    render_inline_notes(text, marks, None)
}

/// `text` with `marks` as inline Markdown, by the one inline renderer; a referenced note is
/// written as the footnote number this export gave it.
fn render_inline_notes(
    text: &str,
    marks: &[MarkSpan],
    footnotes: Option<&mut Footnotes<'_>>,
) -> String {
    match footnotes {
        Some(numbers) => {
            crate::inline::render_with(text, marks, crate::inline::Style::DOCUMENT, |id| {
                let n = numbers.number(id);
                Some(format!("[^{n}]"))
            })
        }
        None => crate::inline::render_with(text, marks, crate::inline::Style::DOCUMENT, |_| None),
    }
}
