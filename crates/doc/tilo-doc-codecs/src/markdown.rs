//! Built-in Markdown codec: maps the modeled rich-text subset (headings, paragraphs,
//! lists, marks, tables, footnotes, code blocks) onto the canonical document. Export
//! normalizes whitespace; unmodeled constructs are not preserved losslessly.
//!
//! Construct → model mapping: `[text](url)` → `link` mark; pipe tables → `cell`
//! blocks tagged `table`/`row`/`col` (header = row 0); indented items → `list_item`
//! `level` attr; `- [ ]`/`- [x]` → `checked` attr; `` `code` `` → `fontFamily`=mono;
//! fences → `code`=true block (+ optional `lang`).

use crate::format::{CodecError, FormatProvider};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::collections::BTreeMap;
use tilo_content::{ContentInputs, ContentSource};
use tilo_doc::document::{MarkSpan, MarkValue};
use tilo_doc::export::{CellGrid, ExportBlock, InlinePlan};
use tilo_doc::model::{DocMut, DocRef};
use tilo_doc::schema::{
    MARK_NOTE_REF, PROP_AUTHOR, PROP_KEYWORDS, PROP_MODIFIED, PROP_SUBJECT, PROP_TITLE,
};
use tilo_doc::snapshot::{BlockView, NoteView};
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

pub struct MarkdownCodec;

impl FormatProvider for MarkdownCodec {
    fn import(
        &self,
        bytes: &[u8],
        doc: &mut DocMut,
        _inputs: &mut ContentInputs,
    ) -> Result<(), CodecError> {
        // Markdown embeds no binary content; images are links.
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

/// Front-matter keys we round-trip, paired with the model property name each maps
/// onto (`date`→`modified`, matching `.odt`/`.docx`'s `dc:date`/`dcterms:modified`; `lang`/`language`→`lang`).
const FRONT_MATTER_MAP: [(&str, &str); 6] = [
    ("title", PROP_TITLE),
    ("author", PROP_AUTHOR),
    ("date", PROP_MODIFIED),
    ("subject", PROP_SUBJECT),
    ("keywords", PROP_KEYWORDS),
    ("lang", "lang"),
];

/// Resolves a front-matter key (case-insensitive, with aliases) to its model property name, or `None` if unmodeled.
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
    // `idx`/`body_start` are sums of `line.len()` from `split_inclusive('\n')`, always valid
    // UTF-8 boundaries into `rest`; `.get()` still guards against a future change breaking that.
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

/// Strips a layer of matching quotes from a scalar value, unescaping `\"` (inverse of [`render_front_matter`]).
fn strip_scalar_quotes(v: &str) -> String {
    if let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return inner.replace("\\\"", "\"");
    }
    if let Some(inner) = v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return inner.to_string();
    }
    v.to_string()
}

/// Emits a `---` front-matter block from the document's well-known properties (empty
/// if none set); inverse of [`apply_front_matter`]. Values containing `:` are quoted.
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

/// Opens a fresh top-level block anchored after the previous one, and makes it current.
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

/// A formatting mark gathered during parsing, applied after the parse so trailing text doesn't expand it (`value` is `None` for boolean toggles).
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

/// Normalises a CSS colour to canonical `#rrggbb` (lower-case hex, common keywords, else trimmed passthrough).
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
    // Open inline `<sup>`/`<sub>` wrappers — the super/subscript marks Markdown has no native syntax for.
    wrap_stack: Vec<(&'static str, usize)>,
    pending_marks: Vec<PendingMark>,
    table_id: Option<String>,
    trow: usize,
    tcol: usize,
    table_seq: usize,
    // Footnote state: a `[^id]` reference lazily creates a note; defs may precede/follow
    // refs, so notes are keyed by label and wired up after the parse.
    note_ids: BTreeMap<String, String>,
    ref_sites: Vec<(String, usize, String)>,
    footnote_bodies: BTreeMap<String, Vec<String>>,
    def_label: Option<String>,
    def_para: Option<String>,
}

impl ImportState {
    /// Id for the note a footnote label refers to, creating (and registering) it on first mention.
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
            // Nested footnote refs inside a definition aren't modeled; everything else here is ignored.
            _ => {}
        }
        true
    }

    /// Block-structure events: headings, paragraphs, lists, tables, code blocks.
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
            Event::Start(Tag::Paragraph) => {
                if !self.in_item && !self.in_cell {
                    start_block(
                        doc,
                        &mut self.prev,
                        &mut self.cur,
                        &mut self.cur_len,
                        "paragraph",
                    );
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if !self.in_item && !self.in_cell {
                    self.cur = None;
                }
            }
            Event::Start(Tag::List(start)) => self.ordered_stack.push(start.is_some()),
            Event::End(TagEnd::List(_)) => {
                self.ordered_stack.pop();
            }
            Event::Start(Tag::Item) => {
                let ordered = *self.ordered_stack.last().unwrap_or(&false);
                // Nesting depth = list stack depth - 1 (top-level lists are level 0).
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

    /// Table and fenced-code-block events (part of [`Self::handle_block_event`],
    /// split out to keep each half under the line-count lint).
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
            // A footnote definition: switches into definition-capture mode; wires its body to the note after the parse.
            Event::Start(Tag::FootnoteDefinition(label)) => {
                let label = label.to_string();
                // Ensure the note exists even without a reference, mirroring CommonMark's lenience.
                let _ = self.note_id_for(doc, &label);
                self.def_label = Some(label);
                self.def_para = None;
            }
            // A footnote reference: places a `note-ref` marker at the current position, deferred so trailing text doesn't expand it.
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
            // A hyperlink: models the href as a `link` mark over the linked range (mirrors .odt, so links round-trip and merge).
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
            Event::Text(t) => {
                if let Some(b) = self.cur.as_ref() {
                    doc.splice_text(b, self.cur_len, 0, t);
                    self.cur_len = self.cur_len.saturating_add(t.chars().count());
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
                if let Some(b) = self.cur.as_ref() {
                    doc.splice_text(b, self.cur_len, 0, " ");
                    self.cur_len = self.cur_len.saturating_add(1);
                }
            }
            // Inline HTML carries colour/size/font marks via `<span style=…>` (emitted by `render_inline`); each tag is its own event, tracked on a stack.
            Event::InlineHtml(html) | Event::Html(html) => self.handle_inline_html(html),
            _ => {}
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
                // `fontSize` is an integer mark (points); other valued marks are strings.
                Some(v) if pm.name == "fontSize" => {
                    if let Ok(n) = v.parse::<i64>() {
                        doc.add_mark(&pm.block, pm.start, pm.end, pm.name, n);
                    }
                }
                Some(v) => doc.add_mark(&pm.block, pm.start, pm.end, pm.name, v.as_str()),
                None => doc.add_mark(&pm.block, pm.start, pm.end, pm.name, true),
            }
        }

        // Cover each footnote-reference placeholder with a `note-ref` mark valued with its note id (indices already settled).
        for (block, idx, note_id) in self.ref_sites {
            let end = idx.saturating_add(1);
            doc.add_mark(&block, idx, end, MARK_NOTE_REF, note_id.as_str());
        }

        // Fill each note's body from its definition's paragraphs; defs with no matching id are dropped.
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
    // Kept inline elements without visible text (drawings, formulas) have no Markdown.
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
                    // Fenced code block: emit content verbatim (no inline escaping),
                    // fenced longer than any fence the code itself shows.
                    let lang = b.str_attr("lang").unwrap_or("");
                    let fence = "`".repeat(fence_length(&b.text));
                    out.push_str(&fence);
                    out.push_str(lang);
                    out.push('\n');
                    out.push_str(&b.text);
                    if !b.text.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&fence);
                } else {
                    out.push_str(&render_inline_notes(
                        &b.text,
                        &b.marks,
                        Some(&mut footnotes),
                    ));
                }
            }
        }
        prev_kind = Some(&b.kind);
        i = i.saturating_add(1);
    }
    out.push('\n');
    // Append `[^N]: …` footnote definitions for every referenced note, after a blank-line separator when there's body content.
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

/// The backticks a code block's fence takes: three, or one more than the
/// longest line of `code` that is only backticks (surrounding whitespace
/// aside), since such a line would close a shorter fence and end the block
/// early. Markdown closes a fence on a line indented up to three spaces,
/// such as the fence of a list item the code shows.
fn fence_length(code: &str) -> usize {
    code.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.bytes().all(|byte| byte == b'`'))
        .map(str::len)
        .max()
        .map_or(3, |longest| longest.saturating_add(1).max(3))
}

/// Escapes a cell so it survives one table row; re-import unescapes `\|` back to `|`.
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
    // Drop the trailing newline; block separators handle inter-block spacing.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Markdown delimiter for a toggle-style mark; `link` wraps rather than toggles, `fontFamily`=mono is the inline-code stand-in.
fn mark_delim(m: &MarkSpan) -> Option<&'static str> {
    match m.name.as_str() {
        "strong" => Some("**"),
        "em" => Some("*"),
        "strike" => Some("~~"),
        "fontFamily" if m.value == MarkValue::Str("mono".to_string()) => Some("`"),
        _ => None,
    }
}

/// Markdown has no native colour/font-size/font-family syntax, so these run marks ride
/// an inline `<span style=…>` on export/import (kept lossless with `.odt`/`.docx`);
/// `fontFamily`=mono is excluded. CSS declaration for a styling mark at char `i`, if any.
fn span_decl(m: &MarkSpan) -> Option<String> {
    match m.name.as_str() {
        "color" => m.value.as_str().map(|c| format!("color: {c}")),
        // Highlight has no `<mark>` colour control, so use a `background` declaration (parsed back via `marks_from_span_style`).
        "highlight" => m.value.as_str().map(|c| format!("background: {c}")),
        "fontSize" => m.value.as_int().map(|n| format!("font-size: {n}pt")),
        "fontFamily" => match m.value.as_str() {
            // mono stays the backtick form; sans/serif map to a generic CSS family.
            Some("serif") => Some("font-family: serif".into()),
            Some("sans") => Some("font-family: sans-serif".into()),
            _ => None,
        },
        _ => None,
    }
}

/// Combined `style="…"` value for a run's styling marks; sorted for a
/// stable, idempotent round-trip.
fn span_style(marks: &[&MarkSpan]) -> Option<String> {
    let mut decls: Vec<String> = marks.iter().filter_map(|mark| span_decl(mark)).collect();
    if decls.is_empty() {
        return None;
    }
    decls.sort();
    decls.dedup();
    Some(decls.join("; "))
}

/// Vertical wrapper (`sup`/`sub`) for a run; `super` wins on overlap (which
/// should not occur in modeled data).
fn vert_wrap(marks: &[&MarkSpan]) -> Option<&'static str> {
    let mut sup = false;
    let mut sub = false;
    for m in marks {
        match m.name.as_str() {
            "super" => sup = true,
            "sub" => sub = true,
            _ => {}
        }
    }
    if sup {
        Some("sup")
    } else if sub {
        Some("sub")
    } else {
        None
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

/// Re-emits inline marks as Markdown, without footnote handling (used for table cells).
fn render_inline(text: &str, marks: &[MarkSpan]) -> String {
    render_inline_notes(text, marks, None)
}

/// Re-emits inline marks as Markdown: closes ending toggle marks, handles the link
/// transition, then opens starting ones; a `note-ref` char renders as `[^N]` via `footnotes`.
fn render_inline_notes(
    text: &str,
    marks: &[MarkSpan],
    mut footnotes: Option<&mut Footnotes>,
) -> String {
    let plan = InlinePlan::new(text, marks);
    let n = text.chars().count();
    let cap = n.saturating_add(1);
    let mut open_at: Vec<Vec<&'static str>> = vec![Vec::new(); cap];
    let mut close_at: Vec<Vec<&'static str>> = vec![Vec::new(); cap];
    for m in marks {
        if let Some(d) = mark_delim(m) {
            let s = m.start.min(n);
            let e = m.end.min(n);
            if s >= e {
                continue;
            }
            if let Some(slot) = open_at.get_mut(s) {
                slot.push(d);
            }
            if let Some(slot) = close_at.get_mut(e) {
                slot.push(d);
            }
        }
    }
    let mut out = String::new();
    let mut cur_link: Option<&str> = None;
    let mut cur_span: Option<String> = None;
    let mut cur_wrap: Option<&'static str> = None;
    for point in plan.points() {
        for d in close_at.get(point.offset).into_iter().flatten() {
            out.push_str(d);
        }
        let span = point.run.as_ref().and_then(|run| span_style(&run.marks));
        let wrap = point.run.as_ref().and_then(|run| vert_wrap(&run.marks));
        let link = point.run.as_ref().and_then(|run| run.link);
        let link_changing = link != cur_link;
        // Close the vertical wrapper first (innermost) when it, the span, or the link changes.
        if (link_changing || span != cur_span || wrap != cur_wrap)
            && let Some(w) = cur_wrap.take()
        {
            let _ = write!(out, "</{w}>");
        }
        if (link_changing || span != cur_span) && cur_span.is_some() {
            out.push_str("</span>");
            cur_span = None;
        }
        if link_changing {
            if let Some(href) = cur_link {
                out.push_str("](");
                out.push_str(href);
                out.push(')');
            }
            cur_link = link;
            if cur_link.is_some() {
                out.push('[');
            }
        }
        if cur_span.is_none()
            && let Some(style) = &span
        {
            let _ = write!(out, r#"<span style="{style}">"#);
            cur_span = Some(style.clone());
        }
        if cur_wrap.is_none()
            && let Some(w) = wrap
        {
            let _ = write!(out, "<{w}>");
            cur_wrap = Some(w);
        }
        for d in open_at.get(point.offset).into_iter().flatten() {
            out.push_str(d);
        }
        if let Some(run) = &point.run {
            if let (Some(fns), Some(note_id)) = (footnotes.as_deref_mut(), run.note_reference) {
                let num = fns.number(note_id);
                let _ = write!(out, "[^{num}]");
            } else {
                out.push_str(run.text);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_doc::Document;
    use tilo_doc::snapshot::{AttrVal, DocSnapshot};

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn markdown_block_structure_roundtrips() {
        {
            let cases = [
                "---\ntitle: \"Notes: café\"\nauthor: Ada\nlang: fr\n---\n\n# Résumé\n\nHello **世界** and *é* with [link](https://tilo.dev/).\n",
                "| Name | Value |\n| --- | --- |\n| **α** | café |\n\n```rust\nlet café = 1;\n```\n",
                "Text[^second] and again[^second], then[^first].\n\n[^first]: First note.\n\n[^second]: Unicode café note.\n",
                "- [x] done\n- [ ] pending\n\n1. ordered\n2. next\n",
                "",
            ];
            for source in cases {
                let mut document = Document::new();
                MarkdownCodec
                    .import(
                        source.as_bytes(),
                        &mut document.as_mut(),
                        &mut tilo_content::ContentInputs::new(),
                    )
                    .unwrap();
                let expected = MarkdownCodec
                    .export(document.as_ref(), &tilo_content::NoContent)
                    .unwrap();
                let snapshot = document.materialize();
                // Export stays usable after the CRDT is gone: these are the complete,
                // already materialized inputs, not a handle that can re-read it.
                drop(document);
                let actual = export_views(&snapshot.properties, &snapshot.blocks, &snapshot.notes);
                assert_eq!(actual, expected, "source: {source:?}");
                let text = String::from_utf8(actual).unwrap();
                if source.contains("[^second]") {
                    assert!(text.contains("[^1]: Unicode café note."), "{text}");
                    assert!(text.contains("[^2]: First note."), "{text}");
                }
                if source.contains("```rust") {
                    assert!(text.contains("```rust\nlet café = 1;\n```"), "{text}");
                    assert!(text.contains("**α**"), "{text}");
                }
                if source.contains("Résumé") {
                    assert!(text.contains("title: \"Notes: café\""), "{text}");
                    assert!(text.contains("**世界**"), "{text}");
                }
            }
        }
        {
            let src = "# Title\n\nHello **world** and *em*.\n\n- a\n- b\n";
            let mut d = Document::new();
            MarkdownCodec
                .import(
                    src.as_bytes(),
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();
            assert_eq!(snap.blocks[0].kind, "heading");
            assert_eq!(snap.blocks[0].text, "Title");
            assert!(
                snap.blocks
                    .iter()
                    .any(|b| b.kind == "paragraph" && b.marks.iter().any(|m| m.name == "strong"))
            );
            assert_eq!(
                snap.blocks.iter().filter(|b| b.kind == "list_item").count(),
                2
            );

            let out1 = MarkdownCodec
                .export(d.as_ref(), &tilo_content::NoContent)
                .unwrap();
            let mut d2 = Document::new();
            MarkdownCodec
                .import(
                    &out1,
                    &mut d2.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let out2 = MarkdownCodec
                .export(d2.as_ref(), &tilo_content::NoContent)
                .unwrap();
            assert_eq!(
                String::from_utf8(out1).unwrap(),
                String::from_utf8(out2).unwrap(),
                "export must be idempotent under re-import"
            );
        }
        {
            let mut d = Document::new();
            MarkdownCodec
                .import(
                    b"## Sub",
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let out = String::from_utf8(
                MarkdownCodec
                    .export(d.as_ref(), &tilo_content::NoContent)
                    .unwrap(),
            )
            .unwrap();
            assert!(out.starts_with("## Sub"), "got {out:?}");
        }
        // Phase `ordered_list_marker`.
        {
            let mut d = Document::new();
            MarkdownCodec
                .import(
                    b"1. one\n2. two\n",
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let out = String::from_utf8(
                MarkdownCodec
                    .export(d.as_ref(), &tilo_content::NoContent)
                    .unwrap(),
            )
            .unwrap();
            assert!(out.contains("1. one"), "got {out:?}");
        }
        {
            let src = "| a | b |\n| --- | --- |\n| 1 | 2 |\n";
            let snap = import(src);
            let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
            assert_eq!(cells.len(), 4, "2x2 cells: {snap:?}");
            assert_eq!(
                cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(),
                vec!["a", "b", "1", "2"]
            );
            let tid = match cells[0].attrs.get("table") {
                Some(AttrVal::Str(s)) => s.clone(),
                other => panic!("cell must carry a table id, got {other:?}"),
            };
            assert!(
                cells
                    .iter()
                    .all(|c| matches!(c.attrs.get("table"), Some(AttrVal::Str(s)) if *s == tid))
            );
            assert_eq!(cells[0].attrs.get("row"), Some(&AttrVal::Int(0)));
            assert_eq!(cells[0].attrs.get("col"), Some(&AttrVal::Int(0)));
            assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
            assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));

            let out = export(src);
            assert!(out.contains("| a | b |"), "header row missing: {out:?}");
            assert!(
                out.contains("| --- | --- |"),
                "delimiter row missing: {out:?}"
            );
            assert!(out.contains("| 1 | 2 |"), "body row missing: {out:?}");
            assert_eq!(export(&out), out, "table export idempotent");
        }
        {
            let src = "- top\n  - sub\n  - sub2\n- top2\n";
            let snap = import(src);
            let items: Vec<_> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "list_item")
                .collect();
            assert_eq!(items.len(), 4);
            let level = |b: &BlockView| match b.attrs.get("level") {
                Some(AttrVal::Int(n)) => *n,
                _ => 0,
            };
            assert_eq!(
                items
                    .iter()
                    .map(|b| (b.text.clone(), level(b)))
                    .collect::<Vec<_>>(),
                vec![
                    ("top".to_string(), 0),
                    ("sub".to_string(), 1),
                    ("sub2".to_string(), 1),
                    ("top2".to_string(), 0),
                ]
            );

            let out = export(src);
            assert!(
                out.contains("- top\n  - sub\n  - sub2\n- top2"),
                "re-indent failed: {out:?}"
            );
            assert_eq!(export(&out), out, "nested-list export idempotent");
        }
        {
            let src = "- [ ] todo\n- [x] done\n";
            let snap = import(src);
            let items: Vec<_> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "list_item")
                .collect();
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].attrs.get("checked"), Some(&AttrVal::Bool(false)));
            assert_eq!(items[1].attrs.get("checked"), Some(&AttrVal::Bool(true)));

            let out = export(src);
            assert!(out.contains("- [ ] todo"), "unchecked box lost: {out:?}");
            assert!(out.contains("- [x] done"), "checked box lost: {out:?}");
            assert_eq!(export(&out), out, "task-list export idempotent");
        }
        {
            let src = "```rust\nlet x = 1;\nlet y = 2;\n```\n";
            let snap = import(src);
            let code = snap
                .blocks
                .iter()
                .find(|b| matches!(b.attrs.get("code"), Some(AttrVal::Bool(true))))
                .expect("fenced block tagged code=true");
            assert_eq!(code.text, "let x = 1;\nlet y = 2;\n");
            assert_eq!(code.attrs.get("lang"), Some(&AttrVal::Str("rust".into())));

            let out = export(src);
            assert!(
                out.contains("```rust\nlet x = 1;\nlet y = 2;\n```"),
                "code block lost: {out:?}"
            );
            assert_eq!(export(&out), out, "fenced-code export idempotent");
        }
    }

    fn import(src: &str) -> DocSnapshot {
        let mut d = Document::new();
        MarkdownCodec
            .import(
                src.as_bytes(),
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        d.materialize()
    }

    fn export(snap_src: &str) -> String {
        let mut d = Document::new();
        MarkdownCodec
            .import(
                snap_src.as_bytes(),
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        String::from_utf8(
            MarkdownCodec
                .export(d.as_ref(), &tilo_content::NoContent)
                .unwrap(),
        )
        .unwrap()
    }

    /// Code that shows a fence (Markdown about Markdown) exports inside a
    /// longer fence, so it reads back as the one code block it was.
    #[test]
    fn a_code_block_showing_a_fence_exports_as_one_block() {
        let src = "````md\n```rust\nlet a = 1;\n```\n````\n\nafter\n";
        let shape = |snapshot: &DocSnapshot| {
            snapshot
                .blocks
                .iter()
                .map(|block| (block.kind.clone(), block.text.clone(), block.flag("code")))
                .collect::<Vec<_>>()
        };
        let imported = import(src);
        assert!(imported.blocks[0].text.contains("```rust"), "{src:?}");
        let out = export(src);
        assert_eq!(shape(&import(&out)), shape(&imported), "exported {out:?}");
        assert_eq!(export(&out), out);
        assert!(export("```sh\nls\n```\n").contains("```sh\nls\n```"));
        // A fence the code shows indented, as under a list item, closes a
        // three-backtick fence too.
        let src = "````md\n- step\n  ```sh\n  ls\n  ```\nmore\n````\n\nafter\n";
        let imported = import(src);
        assert!(imported.blocks[0].text.contains("  ```\nmore"), "{src:?}");
        let out = export(src);
        assert_eq!(shape(&import(&out)), shape(&imported), "exported {out:?}");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn markdown_inline_marks_roundtrip() {
        {
            let src = "See [the docs](https://tilo.so/) now.\n";
            let snap = import(src);
            let p = &snap.blocks[0];
            assert_eq!(p.kind, "paragraph");
            assert_eq!(p.text, "See the docs now.");
            let link = p
                .marks
                .iter()
                .find(|m| m.name == "link")
                .expect("link mark present");
            assert_eq!(link.value, MarkValue::Str("https://tilo.so/".into()));
            assert_eq!((link.start, link.end), (4, 12));

            let out = export(src);
            assert!(out.contains("[the docs](https://tilo.so/)"), "got {out:?}");
            assert_eq!(export(&out), out);
        }
        {
            let src = "run `cargo test` please\n";
            let snap = import(src);
            let p = &snap.blocks[0];
            assert_eq!(p.text, "run cargo test please");
            let mono = p
                .marks
                .iter()
                .find(|m| m.name == "fontFamily")
                .expect("inline code → fontFamily mark");
            assert_eq!(mono.value, MarkValue::Str("mono".into()));
            assert_eq!((mono.start, mono.end), (4, 14));

            let out = export(src);
            assert!(out.contains("`cargo test`"), "got {out:?}");
            assert_eq!(export(&out), out, "inline-code export idempotent");
        }
        {
            // Markdown has no native colour/size/font syntax; these ride as an inline `<span style=…>` on export/import.
            let mut d = Document::new();
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "styled word");
            {
                let mut dm = d.as_mut();
                dm.add_mark(&id, 0, 6, "color", "#ff0000");
                dm.add_mark(&id, 0, 6, "fontSize", 20_i64);
                dm.add_mark(&id, 0, 6, "fontFamily", "serif");
            }
            let out = String::from_utf8(
                MarkdownCodec
                    .export(d.as_ref(), &tilo_content::NoContent)
                    .unwrap(),
            )
            .unwrap();
            assert!(
                out.contains("<span style="),
                "emits an inline span: {out:?}"
            );
            assert!(out.contains("color: #ff0000"), "colour decl: {out:?}");
            assert!(out.contains("font-size: 20pt"), "size decl: {out:?}");
            assert!(out.contains("font-family: serif"), "font decl: {out:?}");

            let mut d2 = Document::new();
            MarkdownCodec
                .import(
                    out.as_bytes(),
                    &mut d2.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let b = &d2.materialize().blocks[0];
            assert_eq!(b.text, "styled word");
            assert!(
                b.marks.iter().any(|m| m.name == "color"
                    && m.value == MarkValue::Str("#ff0000".into())
                    && m.start == 0
                    && m.end == 6),
                "colour mark survives: {:?}",
                b.marks
            );
            assert!(
                b.marks.iter().any(|m| m.name == "fontSize"
                    && m.value == MarkValue::Int(20)
                    && m.start == 0
                    && m.end == 6),
                "size mark survives as Int: {:?}",
                b.marks
            );
            assert!(
                b.marks.iter().any(|m| m.name == "fontFamily"
                    && m.value == MarkValue::Str("serif".into())
                    && m.start == 0
                    && m.end == 6),
                "font mark survives: {:?}",
                b.marks
            );

            assert_eq!(export(&out), out, "span export idempotent");
        }
        {
            // Markdown has no native highlight/super/subscript syntax; highlight rides as `<span style="background:…">`, super/sub as `<sup>`/`<sub>`.
            let mut d = Document::new();
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "x ab cd");
            {
                let mut dm = d.as_mut();
                dm.add_mark(&id, 2, 4, "highlight", "#ffff00"); // "ab"
                dm.add_mark(&id, 5, 6, "super", true); // "c"
                dm.add_mark(&id, 6, 7, "sub", true); // "d"
            }
            let out = String::from_utf8(
                MarkdownCodec
                    .export(d.as_ref(), &tilo_content::NoContent)
                    .unwrap(),
            )
            .unwrap();
            assert!(
                out.contains(r#"<span style="background: #ffff00">"#),
                "highlight span: {out:?}"
            );
            assert!(
                out.contains("<sup>") && out.contains("</sup>"),
                "sup wrapper: {out:?}"
            );
            assert!(
                out.contains("<sub>") && out.contains("</sub>"),
                "sub wrapper: {out:?}"
            );

            let mut d2 = Document::new();
            MarkdownCodec
                .import(
                    out.as_bytes(),
                    &mut d2.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let b = &d2.materialize().blocks[0];
            assert_eq!(b.text, "x ab cd");
            assert!(
                b.marks.iter().any(|m| m.name == "highlight"
                    && m.value == MarkValue::Str("#ffff00".into())
                    && m.start == 2
                    && m.end == 4),
                "highlight mark survives: {:?}",
                b.marks
            );
            assert!(
                b.marks.iter().any(|m| m.name == "super"
                    && m.value == MarkValue::Bool(true)
                    && m.start == 5
                    && m.end == 6),
                "super mark survives: {:?}",
                b.marks
            );
            assert!(
                b.marks.iter().any(|m| m.name == "sub"
                    && m.value == MarkValue::Bool(true)
                    && m.start == 6
                    && m.end == 7),
                "sub mark survives: {:?}",
                b.marks
            );

            assert_eq!(export(&out), out, "highlight/super/sub export idempotent");
        }
        {
            let src = "this is ~~gone~~ ok\n";
            let snap = import(src);
            let p = &snap.blocks[0];
            assert_eq!(p.text, "this is gone ok");
            assert!(
                p.marks.iter().any(|m| m.name == "strike"),
                "strike mark present: {:?}",
                p.marks
            );
            let out = export(src);
            assert!(out.contains("~~gone~~"), "got {out:?}");
            assert_eq!(export(&out), out);
        }
    }

    #[test]
    fn markdown_front_matter_maps_to_document_properties() {
        {
            let src = "---\ntitle: My Doc\nauthor: Ada Lovelace\ndate: 2026-06-30\nlang: en-GB\n---\n\nBody text.\n";
            let snap = import(src);
            assert_eq!(
                snap.properties.get(PROP_TITLE).map(String::as_str),
                Some("My Doc")
            );
            assert_eq!(
                snap.properties.get(PROP_AUTHOR).map(String::as_str),
                Some("Ada Lovelace")
            );
            assert_eq!(
                snap.properties.get(PROP_MODIFIED).map(String::as_str),
                Some("2026-06-30")
            );
            assert_eq!(
                snap.properties.get("lang").map(String::as_str),
                Some("en-GB")
            );
            assert_eq!(snap.blocks.len(), 1, "only the body paragraph: {snap:?}");
            assert_eq!(snap.blocks[0].text, "Body text.");
        }
        {
            let src = "---\ntitle: \"Notes: A Sequel\"\n---\n\nHi.\n";
            let snap = import(src);
            assert_eq!(
                snap.properties.get(PROP_TITLE).map(String::as_str),
                Some("Notes: A Sequel")
            );
            let out = export(src);
            assert!(
                out.starts_with("---\n"),
                "front-matter emitted first: {out:?}"
            );
            assert!(
                out.contains("title: \"Notes: A Sequel\""),
                "colon value quoted: {out:?}"
            );
            assert_eq!(export(&out), out, "front-matter export idempotent");
        }
        {
            let mut d = Document::new();
            {
                let mut dm = d.as_mut();
                dm.set_doc_prop(PROP_TITLE, "T");
                dm.set_doc_prop(PROP_AUTHOR, "A");
            }
            let _ = d.insert_block(None, "paragraph");
            let out = String::from_utf8(
                MarkdownCodec
                    .export(d.as_ref(), &tilo_content::NoContent)
                    .unwrap(),
            )
            .unwrap();
            assert!(
                out.starts_with("---\ntitle: T\nauthor: A\n---\n\n"),
                "got {out:?}"
            );
        }
        {
            let src = "---\ntitle: Round Trip\nauthor: Grace\n---\n\nHello world.\n";
            let out1 = export(src);
            let snap = import(&out1);
            assert_eq!(
                snap.properties.get(PROP_TITLE).map(String::as_str),
                Some("Round Trip")
            );
            assert_eq!(
                snap.properties.get(PROP_AUTHOR).map(String::as_str),
                Some("Grace")
            );
            assert_eq!(snap.blocks[0].text, "Hello world.");
            assert_eq!(export(&out1), out1, "front-matter+body export idempotent");
        }
        {
            // No properties → no front-matter block; a leading `---` in a property-less doc is just a thematic break.
            let src = "Just a paragraph.\n";
            let out = export(src);
            assert!(!out.starts_with("---"), "no spurious front-matter: {out:?}");
            assert_eq!(out, "Just a paragraph.\n");

            // An unterminated `---` opening is treated as body (no properties set).
            let snap = import("---\ntitle: x\n\nno close fence\n");
            assert!(
                snap.properties.is_empty(),
                "unterminated block sets no props: {:?}",
                snap.properties
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn markdown_footnotes_roundtrip_in_reference_order() {
        {
            let src = "Text with a ref[^1] here.\n\n[^1]: The footnote body.\n";
            let snap = import(src);
            assert_eq!(
                snap.notes.len(),
                1,
                "exactly one footnote: {:?}",
                snap.notes
            );
            let note = &snap.notes[0];
            assert_eq!(note.kind, "footnote");
            assert_eq!(note.blocks.len(), 1, "one body paragraph");
            assert_eq!(note.blocks[0].text, "The footnote body.");

            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("body paragraph");
            let nref = p
                .marks
                .iter()
                .find(|m| m.name == MARK_NOTE_REF)
                .expect("note-ref mark present");
            assert_eq!(
                nref.value,
                MarkValue::Str(note.id.clone()),
                "note-ref points at the note id"
            );
            // The placeholder sits at the reference position ("Text with a ref" = 15 chars).
            assert_eq!((nref.start, nref.end), (15, 16));
            assert_eq!(
                p.text.chars().nth(15),
                Some('\u{FFFC}'),
                "placeholder spliced at ref site"
            );
        }
        {
            let src = "A para with note[^1] inside.\n\n[^1]: body text\n";
            let out = export(src);
            assert!(
                out.contains("A para with note[^1] inside."),
                "ref position: {out:?}"
            );
            assert!(
                out.contains("[^1]: body text"),
                "definition appended: {out:?}"
            );

            let snap = import(&out);
            assert_eq!(
                snap.notes.len(),
                1,
                "one footnote survives: {:?}",
                snap.notes
            );
            assert_eq!(snap.notes[0].blocks[0].text, "body text");
            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("body paragraph");
            assert!(
                p.marks.iter().any(|m| m.name == MARK_NOTE_REF),
                "note-ref survives: {:?}",
                p.marks
            );
            assert_eq!(export(&out), out, "footnote export idempotent");
        }
        {
            // Headline scenario: front-matter + a footnote, exported then re-imported, must preserve both.
            let mut d = Document::new();
            {
                let mut dm = d.as_mut();
                dm.set_doc_prop(PROP_TITLE, "Spec");
                dm.set_doc_prop(PROP_AUTHOR, "Quinten");
            }
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "See here for details.");
            let nid = {
                let mut dm = d.as_mut();
                // Footnote reference after "See here" (char index 8).
                let nid = dm.insert_footnote(&p, 8);
                let nb = dm.insert_note_block(&nid, None, "paragraph").unwrap();
                dm.splice_note_text(&nid, &nb, 0, 0, "the cited source");
                nid
            };

            let out = String::from_utf8(
                MarkdownCodec
                    .export(d.as_ref(), &tilo_content::NoContent)
                    .unwrap(),
            )
            .unwrap();
            assert!(
                out.starts_with("---\ntitle: Spec\nauthor: Quinten\n---\n\n"),
                "front-matter: {out:?}"
            );
            assert!(
                out.contains("See here[^1] for details."),
                "ref inline: {out:?}"
            );
            assert!(
                out.contains("[^1]: the cited source"),
                "definition: {out:?}"
            );

            let mut d2 = Document::new();
            MarkdownCodec
                .import(
                    out.as_bytes(),
                    &mut d2.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d2.materialize();
            assert_eq!(
                snap.properties.get(PROP_TITLE).map(String::as_str),
                Some("Spec")
            );
            assert_eq!(
                snap.properties.get(PROP_AUTHOR).map(String::as_str),
                Some("Quinten")
            );
            assert_eq!(snap.notes.len(), 1, "footnote survives: {:?}", snap.notes);
            assert_eq!(snap.notes[0].blocks[0].text, "the cited source");
            let p2 = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("body paragraph");
            let nref = p2
                .marks
                .iter()
                .find(|m| m.name == MARK_NOTE_REF)
                .expect("note-ref mark");
            assert_eq!(nref.value, MarkValue::Str(snap.notes[0].id.clone()));
            assert_eq!(nref.start, 8, "ref position preserved");
            // `nid` is local to `d`; the re-imported note gets a fresh id — what matters is the ref/body survive.
            let _ = nid;
        }
        {
            let src = "First[^a] then second[^b].\n\n[^a]: alpha\n[^b]: beta\n";
            let out = export(src);
            // Labels are renumbered 1,2 in reference order regardless of source labels.
            assert!(
                out.contains("First[^1] then second[^2]."),
                "renumbered refs: {out:?}"
            );
            assert!(out.contains("[^1]: alpha"), "first def: {out:?}");
            assert!(out.contains("[^2]: beta"), "second def: {out:?}");
            assert_eq!(export(&out), out, "two-footnote export idempotent");
        }
    }

    use proptest::prelude::*;

    fn doc_strategy() -> impl Strategy<Value = String> {
        let word = "[a-z]{1,5}";
        let inline = prop_oneof![
            word.prop_map(|w| w),
            word.prop_map(|w| format!("**{w}**")),
            word.prop_map(|w| format!("*{w}*")),
        ];
        let line = prop_oneof![
            proptest::collection::vec(inline, 1..4).prop_map(|ws| ws.join(" ")),
            "[a-z]{1,5}".prop_map(|w| format!("# {w}")),
            "[a-z]{1,5}".prop_map(|w| format!("- {w}")),
        ];
        proptest::collection::vec(line, 0..6).prop_map(|lines| lines.join("\n\n") + "\n")
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

        #[test]
        fn export_is_idempotent(src in doc_strategy()) {
            let mut d = Document::new();
            MarkdownCodec.import(src.as_bytes(), &mut d.as_mut(), &mut tilo_content::ContentInputs::new()).unwrap();
            let out1 = MarkdownCodec.export(d.as_ref(), &tilo_content::NoContent).unwrap();
            let mut d2 = Document::new();
            MarkdownCodec.import(&out1, &mut d2.as_mut(), &mut tilo_content::ContentInputs::new()).unwrap();
            let out2 = MarkdownCodec.export(d2.as_ref(), &tilo_content::NoContent).unwrap();
            prop_assert_eq!(String::from_utf8(out1).unwrap(), String::from_utf8(out2).unwrap());
        }
    }
}
