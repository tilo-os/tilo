//! Parse the `content.xml` `office:text` body into canonical blocks + marks against a
//! pre-parsed `StyleRegistry` (named styles from `styles.xml`, automatic from
//! `content.xml`); headings are recognised from `text:h` *and* from a heading
//! paragraph-style. Unmodeled body elements are captured verbatim as fidelity atoms.

use super::styles::{StyleRegistry, attr_val};
use crate::format::CodecError;
use crate::package::ZipPackage;
use crate::preserve::{ATTR_FORMAT, MARK_ODT_XML};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::BTreeMap;
use tilo_content::ContentInputs;
use tilo_doc::document::MarkValue;
use tilo_doc::model::DocMut;
use tilo_doc::schema::{FIELD_PLACEHOLDER, MARK_COMMENT, MARK_NOTE_REF};
use tilo_doc::snapshot::ChangeKind;

/// Parse an ODF length (`svg:width`/`svg:height`, e.g. `"3cm"`, `"96px"`, `"1in"`) to an
/// integer pixel count; `px` round-trips exactly, other units convert at 96dpi.
#[allow(
    clippy::cast_possible_truncation,
    reason = "an ODF length converted to px; realistic document dimensions are far below i64::MAX"
)]
pub(super) fn parse_len_px(v: &str) -> Option<i64> {
    let v = v.trim();
    let split = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let n: f64 = num.trim().parse().ok()?;
    let px = match unit.trim() {
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        "in" => n * 96.0,
        "pt" => n * 96.0 / 72.0,
        "pc" => n * 16.0,
        // "px" | "" and anything unrecognized: already in px.
        _ => n,
    };
    Some(px.round() as i64)
}

/// In-progress `draw:frame` capture: an image, or a verbatim fidelity atom when the picture is missing.
struct FrameState {
    raw_start: usize,
    depth: i32,
    width: Option<String>,
    height: Option<String>,
    /// Model anchor (`inline`|`paragraph`|`page`) from `text:anchor-type`.
    anchor: Option<String>,
    /// Model wrap (`none`|`left`|`right`|`both`) resolved from the frame's graphic style's `style:wrap`.
    wrap: Option<String>,
    href: Option<String>,
    title: String,
    desc: String,
    collecting: Option<bool>, // Some(true)=svg:title, Some(false)=svg:desc
    /// When the frame is nested *inside* a `text:p`, the id of that paragraph block (so an
    /// empty image-only paragraph can be dropped after the image is emitted as its own block).
    enclosing_para: Option<String>,
    para_had_text: bool,
}

impl FrameState {
    /// The picture's alt text, size and positioning, as attrs on its `image` block `id`.
    fn set_image_attrs(&self, doc: &mut DocMut, id: &str) {
        let alt = if self.title.is_empty() {
            &self.desc
        } else {
            &self.title
        };
        if !alt.is_empty() {
            doc.set_attr(id, "alt", alt.as_str());
        }
        if let Some(w) = self.width.as_deref().and_then(parse_len_px) {
            doc.set_attr(id, "width", w);
        }
        if let Some(h) = self.height.as_deref().and_then(parse_len_px) {
            doc.set_attr(id, "height", h);
        }
        // Positioning: anchor (inline/paragraph/page) + wrap.
        if let Some(a) = self.anchor.as_deref() {
            doc.set_attr(id, "anchor", a);
        }
        if let Some(w) = self.wrap.as_deref() {
            doc.set_attr(id, "wrap", w);
        }
    }
}

/// Whether a raw text node is purely pretty-printer indentation (LibreOffice/pandoc insert
/// newlines around structural list/table/heading elements — such nodes always contain a
/// newline, unlike genuine inline whitespace). Dropping these keeps list items and
/// headings tight instead of importing the indentation as blank lines.
pub(super) fn is_layout_whitespace(t: &str) -> bool {
    !t.is_empty() && t.chars().all(char::is_whitespace) && t.contains(['\n', '\r'])
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

/// A `fidelity` block holding `raw`, an unmodeled body element kept verbatim.
fn fidelity_block(
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    raw: &str,
) {
    let id = start_block(doc, prev, cur, cur_len, "fidelity");
    doc.set_attr(&id, "xml", raw);
    doc.set_attr(&id, ATTR_FORMAT, "odt");
}

fn outline_level(e: &BytesStart) -> Option<i64> {
    attr_val(e, b"text:outline-level").and_then(|v| v.parse::<i64>().ok())
}

fn append(doc: &mut DocMut, cur: Option<&str>, cur_len: &mut usize, txt: &str) {
    if let Some(b) = cur
        && !txt.is_empty()
    {
        doc.splice_text(b, *cur_len, 0, txt);
        *cur_len = cur_len.saturating_add(txt.chars().count());
    }
}

/// Open a paragraph or heading depending on the referenced paragraph style, set
/// level/alignment, and return the paragraph-level inline marks to apply over the block.
fn open_para(
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    reg: &StyleRegistry,
    style: Option<&str>,
) -> Vec<(String, MarkValue)> {
    let level = style.and_then(|s| reg.table.heading_level(s, |_| None));
    let id = start_block(
        doc,
        prev,
        cur,
        cur_len,
        if level.is_some() {
            "heading"
        } else {
            "paragraph"
        },
    );
    if let Some(l) = level {
        doc.set_attr(&id, "level", l);
    }
    // Line spacing rides as a direct `line_spacing` attr, not a CRDT named-style prop, so it's always lifted onto the block even when the paragraph references a named style below.
    if let Some(lh) = style.and_then(|s| reg.table.para_line_height(s)) {
        doc.set_attr(&id, "line_spacing", lh);
    }
    // A *named* (common) non-heading paragraph style becomes a real CRDT-native style: lifted
    // into the styles container with the block pointing at it via `style`, instead of also
    // carrying alignment/formatting as direct attrs — keeping the round-trip idempotent.
    if level.is_none()
        && let Some(s) = style
        && reg.table.is_named_para(s)
        && let Some(props) = reg.table.style_props(s, super::styles::decode_style_name)
    {
        let key = super::styles::decode_style_name(s);
        doc.set_style(&key, &props);
        doc.set_attr(&id, "style", key.as_str());
        return Vec::new();
    }
    if let Some(a) = style.and_then(|s| reg.table.para_align(s)) {
        doc.set_attr(&id, "align", a.as_str());
    }
    if level.is_some() {
        Vec::new()
    } else {
        style
            .map(|s| reg.table.para_text_marks(s))
            .unwrap_or_default()
    }
}

/// An open `text:a` hyperlink on [`parse_body`]'s link stack: its `href` (if any), the text
/// marks its own style carries, and the char index where it opened.
type LinkStackEntry = (Option<String>, Vec<(String, MarkValue)>, usize);

impl BodyState {
    /// Advance the in-progress verbatim-capture state machine for one XML event: an unmodeled
    /// element's raw source, re-emitted as a `fidelity` block once its closing tag is reached.
    /// No-op unless `capture` is already `Some` (the caller only calls this while it is).
    fn advance_capture(&mut self, ev: &Event, s: &str, end_pos: usize, doc: &mut DocMut) {
        let Some((start, depth)) = self.capture.as_mut() else {
            return;
        };
        match ev {
            Event::Start(_) => *depth = depth.saturating_add(1),
            Event::End(_) => {
                *depth = depth.saturating_sub(1);
                if *depth == 0 {
                    let raw = s.get(*start..end_pos).unwrap_or_default();
                    fidelity_block(doc, &mut self.prev, &mut self.cur, &mut self.cur_len, raw);
                    self.cur = None;
                    self.capture = None;
                }
            }
            _ => {}
        }
    }

    /// Advance the in-progress `draw:frame` state machine for one XML event: collects the image
    /// href + alt text, then on close emits either an `image` block (if the Pictures part has the
    /// bytes) or a verbatim fidelity atom. No-op unless `frame` is already `Some`.
    fn advance_frame(
        &mut self,
        ev: &Event,
        s: &str,
        end_pos: usize,
        container: &ZipPackage,
        doc: &mut DocMut,
        inputs: &mut ContentInputs,
    ) -> Result<FrameOutcome, CodecError> {
        let Some(fr) = self.frame.as_mut() else {
            return Ok(FrameOutcome::Open);
        };
        match ev {
            Event::Start(e) => {
                fr.depth = fr.depth.saturating_add(1);
                match e.name().as_ref() {
                    b"draw:image" => {
                        if fr.href.is_none() {
                            fr.href = attr_val(e, b"xlink:href");
                        }
                    }
                    b"svg:title" => fr.collecting = Some(true),
                    b"svg:desc" => fr.collecting = Some(false),
                    _ => {}
                }
            }
            Event::Empty(e) => {
                if e.name().as_ref() == b"draw:image" && fr.href.is_none() {
                    fr.href = attr_val(e, b"xlink:href");
                }
            }
            Event::Text(t) => {
                if let Some(is_title) = fr.collecting {
                    let txt = t
                        .xml10_content()
                        .map_err(|e| CodecError::Parse(format!("text: {e}")))?;
                    if is_title {
                        fr.title.push_str(&txt);
                    } else {
                        fr.desc.push_str(&txt);
                    }
                }
            }
            Event::GeneralRef(r) => {
                let resolved = crate::resolve_general_ref(r);
                match fr.collecting {
                    Some(true) => fr.title.extend(resolved),
                    Some(false) => fr.desc.extend(resolved),
                    None => {}
                }
            }
            Event::End(e) => {
                fr.depth = fr.depth.saturating_sub(1);
                match e.name().as_ref() {
                    b"svg:title" | b"svg:desc" => fr.collecting = None,
                    _ => {}
                }
                if fr.depth == 0 {
                    let bytes = fr
                        .href
                        .as_deref()
                        .and_then(|h| container.get(h).map(<[u8]>::to_vec));
                    if let Some(bytes) = bytes {
                        let id = start_block(
                            doc,
                            &mut self.prev,
                            &mut self.cur,
                            &mut self.cur_len,
                            "image",
                        );
                        let hash = doc.attach(&bytes, inputs);
                        doc.set_attr(&id, "blob", hash.as_str());
                        fr.set_image_attrs(doc, &id);
                        // An image-only paragraph (the common LibreOffice/pandoc "figure" layout) leaves an empty wrapper paragraph — drop it so the image isn't preceded by a blank line.
                        if let Some(pid) = fr.enclosing_para.as_deref()
                            && !fr.para_had_text
                        {
                            doc.remove_block(pid);
                        }
                    } else {
                        // Not a picture in the container (text box, formula, chart…): keep it verbatim, in place.
                        let raw = s.get(fr.raw_start..end_pos).unwrap_or_default().to_string();
                        if fr.enclosing_para.is_some() {
                            self.frame = None;
                            return Ok(FrameOutcome::Inline(raw));
                        }
                        fidelity_block(doc, &mut self.prev, &mut self.cur, &mut self.cur_len, &raw);
                    }
                    self.cur = None;
                    self.frame = None;
                    return Ok(FrameOutcome::Block);
                }
            }
            _ => {}
        }
        Ok(FrameOutcome::Open)
    }
}

/// What a `draw:frame` event left behind.
enum FrameOutcome {
    /// The frame is still open.
    Open,
    /// The frame became a block (an `image`, or a verbatim `fidelity` block).
    Block,
    /// The frame, kept verbatim, belongs inline at the current position of its paragraph.
    Inline(String),
}

/// Advance the in-progress inline `text:note` state machine for one XML event: collects its raw
/// XML until it closes, then parses it into a note + records a reference marker at the
/// captured position. No-op unless `note` is already `Some`.
fn advance_note(
    ev: &Event,
    s: &str,
    end_pos: usize,
    note: &mut Option<(usize, i32, Option<String>, usize)>,
    doc: &mut DocMut,
    note_refs: &mut Vec<(String, usize, String)>,
) {
    let Some((start, depth, block, idx)) = note.as_mut() else {
        return;
    };
    match ev {
        Event::Start(_) => *depth = depth.saturating_add(1),
        Event::End(_) => {
            *depth = depth.saturating_sub(1);
            if *depth == 0 {
                let raw = s.get(*start..end_pos).unwrap_or_default().to_string();
                if let Some(b) = block.clone() {
                    let nid = super::notes::import_note(&raw, doc);
                    note_refs.push((b, *idx, nid));
                }
                *note = None;
            }
        }
        _ => {}
    }
}

/// Advance the in-progress inline `office:annotation` (comment) state machine for one XML
/// event: collects its raw XML until it closes, then parses it, stashing a ranged comment
/// until its `annotation-end` or anchoring a point comment immediately. No-op unless
/// `annotation` is already `Some`.
fn advance_annotation(
    ev: &Event,
    s: &str,
    end_pos: usize,
    annotation: &mut Option<(usize, i32, Option<String>, usize)>,
    doc: &mut DocMut,
    open_comments: &mut BTreeMap<String, (String, String, usize)>,
    comment_ranges: &mut Vec<(String, usize, usize, String)>,
) {
    let Some((start, depth, block, idx)) = annotation.as_mut() else {
        return;
    };
    match ev {
        Event::Start(_) => *depth = depth.saturating_add(1),
        Event::End(_) => {
            *depth = depth.saturating_sub(1);
            if *depth == 0 {
                let raw = s.get(*start..end_pos).unwrap_or_default().to_string();
                if let Some(b) = block.clone() {
                    let (name, cid) = super::comments::import_annotation(&raw, doc);
                    match name {
                        // A ranged comment: stash it until its annotation-end.
                        Some(n) => {
                            open_comments.insert(n, (cid, b, *idx));
                        }
                        // A point comment (no office:name): anchor a 1-char range at the insertion point if there's a character there, else a zero-width range that import drops.
                        None => comment_ranges.push((b, *idx, *idx, cid)),
                    }
                }
                *annotation = None;
            }
        }
        _ => {}
    }
}

/// Advance the in-progress `text:tracked-changes` state machine for one XML event: captures its
/// raw XML until it closes, then parses the changed-region declarations into an
/// `xml:id → (change id, kind)` map the body's change-start/-end/change markers reference.
/// No-op unless `tracked` is already `Some`.
fn advance_tracked(
    ev: &Event,
    s: &str,
    end_pos: usize,
    tracked: &mut Option<(usize, i32)>,
    doc: &mut DocMut,
    change_decls: &mut BTreeMap<String, (String, ChangeKind)>,
    deleted_texts: &mut BTreeMap<String, String>,
) {
    let Some((start, depth)) = tracked.as_mut() else {
        return;
    };
    match ev {
        Event::Start(_) => *depth = depth.saturating_add(1),
        Event::End(_) => {
            *depth = depth.saturating_sub(1);
            if *depth == 0 {
                let raw = s.get(*start..end_pos).unwrap_or_default().to_string();
                for decl in super::changes::import_tracked_changes(&raw) {
                    let cid = doc.insert_change(decl.kind, &decl.author, &decl.date);
                    if !decl.deleted.is_empty() {
                        deleted_texts.insert(decl.xml_id.clone(), decl.deleted);
                    }
                    change_decls.insert(decl.xml_id, (cid, decl.kind));
                }
                *tracked = None;
            }
        }
        _ => {}
    }
}

/// All per-parse mutable state for [`parse_body`]'s event loop, bundled so the loop can be split
/// into named handlers below instead of one 500-line function.
#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent XML-parse-state flag, set by name at its own open/close \
              tag handler, never positionally"
)]
struct BodyState {
    in_text: bool,
    prev: Option<String>,
    cur: Option<String>,
    cur_len: usize,
    in_item: bool,

    // Span marks + paragraph-style marks, applied after all text is in (so text appended after a span doesn't expand its mark).
    span_stack: Vec<(Vec<(String, MarkValue)>, usize)>,
    link_stack: Vec<LinkStackEntry>,
    pending_marks: Vec<(String, usize, usize, String, MarkValue)>,
    cur_para_marks: Vec<(String, MarkValue)>,
    list_stack: Vec<bool>,
    // Per-open-list captured numbering for fidelity preservation: level-0 format/start/restart, from the list-style plus the `text:list` element's own start-value/continue-numbering.
    list_num_stack: Vec<(Option<&'static str>, Option<i64>, bool)>,

    table_id: Option<String>,
    trow: usize,
    tcol: usize,
    table_seq: usize,

    capture: Option<(usize, i32)>, // (start byte, open depth) of a fidelity atom
    frame: Option<FrameState>,     // an in-progress draw:frame (image or fidelity)
    // An in-progress inline `text:note`: raw-XML start byte, open depth, and the (block, index)
    // to anchor its reference marker — deferred so it's added once all text is in place.
    note: Option<(usize, i32, Option<String>, usize)>,
    // (block, char index, note id) reference markers, applied after all text is in.
    note_refs: Vec<(String, usize, String)>,
    // An in-progress inline `office:annotation` (comment): raw-XML start byte, open depth, and the (block, start char index) where the commented range begins.
    annotation: Option<(usize, i32, Option<String>, usize)>,
    // Pending comment ranges keyed by ODF `office:name` → (comment id, block, start char index), awaiting the matching `office:annotation-end`.
    open_comments: BTreeMap<String, (String, String, usize)>,
    // (block, start, end, comment id) anchor marks, applied after all text is in.
    comment_ranges: Vec<(String, usize, usize, String)>,

    // Open bookmark ranges keyed by ODF `text:name` → (block, start char index), awaiting the matching `text:bookmark-end`.
    open_bookmarks: BTreeMap<String, (String, usize)>,
    // (block, start, end, bookmark name) anchor marks, applied after all text is in.
    bookmark_ranges: Vec<(String, usize, usize, String)>,

    // An in-progress `text:tracked-changes` block, captured whole then parsed into change records on close.
    tracked: Option<(usize, i32)>,
    // ODF `xml:id` → (qdoc change id, kind), from the parsed tracked-changes block; the body's change-start/-end/change markers reference changes by this id.
    change_decls: BTreeMap<String, (String, ChangeKind)>,
    // ODF `xml:id` → a deletion's deleted text, spliced back in at its point marker.
    deleted_texts: BTreeMap<String, String>,
    // Open tracked-change ranges keyed by ODF `xml:id` → (change id, kind, block, start char index), awaiting the matching `text:change-end`.
    open_changes: BTreeMap<String, (String, ChangeKind, String, usize)>,
    // (block, start, end, kind, change id) anchor marks, applied after all text is in.
    change_ranges: Vec<(String, usize, usize, ChangeKind, String)>,

    // Inside a `text:p`/`text:h`: unmodeled children become inline atoms.
    in_para: bool,
    // A picture frame split the current paragraph: content after it continues in a new block.
    continue_para: bool,
    // Paragraphs opened so far in the current table cell / list item; later ones join with `\n`.
    item_paras: usize,
    // (start byte, open depth) of an in-progress inline atom.
    inline: Option<(usize, i32)>,
    // Grid columns the open (or covered) table cell takes: its `table:number-columns-repeated`.
    cell_repeat: usize,
    // The outermost open list's style.
    list_style: Option<String>,
    // The current table's style and columns, and row's style, until a cell takes them.
    table_style: Option<String>,
    table_columns: Option<String>,
    row_style: Option<String>,
}

/// Block attr: a paragraph's automatic style, referenced again on export while the block's
/// modeled paragraph attributes still agree with it.
pub(super) const ODT_STYLE: &str = "odt_style";
/// Whether an automatic style is one the exporter generates from the model (`qdoc` in its
/// name), which says nothing the model does not.
fn is_generated_style(name: &str) -> bool {
    // Export invents `Lqdoc{n}` list styles (and historically `tilo-doc*`) when the
    // model had no preserved ODT name. Re-import must not persist those as
    // `odt_list_style` or a plain list round-trip grows attrs.
    name.contains("tilo-doc") || name.starts_with("Lqdoc")
}

/// List-item attr: the style of the list it is in, referenced again on export (with every
/// level's numbering and bullets) while the item still numbers the way it says.
pub(super) const ODT_LIST_STYLE: &str = "odt_list_style";
/// Mark: a text style (a character style, or one with properties no mark represents) the
/// covered text is wrapped in again on export.
pub(super) const MARK_ODT_SPAN: &str = "odt_span";
/// Attrs on a table's first cell (its style and `table:table-column` elements), a row's first
/// cell (the row's style) and each cell (its style).
pub(super) const ODT_TABLE_STYLE: &str = "odt_table_style";
pub(super) const ODT_COLUMNS: &str = "odt_columns";
pub(super) const ODT_ROW_STYLE: &str = "odt_row_style";
pub(super) const ODT_CELL_STYLE: &str = "odt_cell_style";

/// A table cell's `table:number-columns-repeated` (at least 1).
fn columns_repeated(e: &BytesStart) -> usize {
    attr_val(e, b"table:number-columns-repeated")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 1024)
}

/// A table cell's span in the `table:number-columns-spanned`/`-rows-spanned` attribute `key`,
/// when it spans more than one.
fn span(e: &BytesStart, key: &[u8]) -> Option<i64> {
    attr_val(e, key)
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n > 1)
}

/// Paragraph content the body parser models; any other element inside a `text:p`/`text:h` is
/// kept verbatim as an inline atom.
const PARA_MODELED: &[&[u8]] = &[
    b"text:span",
    b"text:a",
    b"text:note",
    b"office:annotation",
    b"office:annotation-end",
    b"text:bookmark",
    b"text:bookmark-start",
    b"text:bookmark-end",
    b"text:change",
    b"text:change-start",
    b"text:change-end",
    b"text:tab",
    b"text:line-break",
    b"text:s",
    // A layout hint of where the application last broke the page.
    b"text:soft-page-break",
];

impl BodyState {
    /// After a picture frame split the paragraph, open the block carrying the rest of it.
    fn ensure_continuation(&mut self, doc: &mut DocMut) {
        if self.cur.is_none() && self.continue_para && self.in_para {
            self.continue_para = false;
            start_block(
                doc,
                &mut self.prev,
                &mut self.cur,
                &mut self.cur_len,
                "paragraph",
            );
            for entry in &mut self.span_stack {
                entry.1 = 0;
            }
            for entry in &mut self.link_stack {
                entry.2 = 0;
            }
        }
    }

    /// Record `raw` as an inline atom at the current position: its visible text (or one
    /// placeholder when it shows none, see `preserve::covered_text`), covered by a mark holding
    /// the element verbatim.
    fn push_atom(&mut self, doc: &mut DocMut, raw: String) {
        self.ensure_continuation(doc);
        if let Some(b) = self.cur.clone() {
            let idx = self.cur_len;
            let text = crate::preserve::covered_text(&raw, MARK_ODT_XML);
            append(doc, Some(b.as_str()), &mut self.cur_len, &text);
            self.pending_marks.push((
                b,
                idx,
                self.cur_len,
                MARK_ODT_XML.to_string(),
                MarkValue::Str(raw),
            ));
        }
    }

    /// Open a `cell` block at the current grid position, carrying its spans. A cell repeated
    /// over several columns (an empty run `LibreOffice` writes once) is one block; the columns
    /// after it stay empty.
    fn open_cell(&mut self, e: &BytesStart, doc: &mut DocMut) {
        self.cell_repeat = columns_repeated(e);
        let Some(tid) = self.table_id.clone() else {
            return;
        };
        let id = start_block(
            doc,
            &mut self.prev,
            &mut self.cur,
            &mut self.cur_len,
            "cell",
        );
        doc.set_attr(&id, "table", tid.as_str());
        doc.set_attr(&id, "row", i64::try_from(self.trow).unwrap_or(i64::MAX));
        doc.set_attr(&id, "col", i64::try_from(self.tcol).unwrap_or(i64::MAX));
        if let Some(n) = span(e, b"table:number-columns-spanned") {
            doc.set_attr(&id, "colspan", n);
        }
        if let Some(n) = span(e, b"table:number-rows-spanned") {
            doc.set_attr(&id, "rowspan", n);
        }
    }

    /// A second or later paragraph of a list item or table cell continues its text on a new line.
    fn join_item_paragraph(&mut self, doc: &mut DocMut) {
        if self.item_paras > 0 {
            append(doc, self.cur.as_deref(), &mut self.cur_len, "\n");
        }
        self.item_paras = self.item_paras.saturating_add(1);
    }

    /// Whether an element starting here is paragraph content the model does not represent.
    /// A `draw:frame` in a top-level paragraph may be a picture and is decided when it closes.
    fn is_unmodeled_para_child(&self, name: &[u8]) -> bool {
        if !self.in_text || !self.in_para || PARA_MODELED.contains(&name) {
            return false;
        }
        name != b"draw:frame" || self.in_item || self.table_id.is_some()
    }

    /// While an inline atom is being captured, consume events until its end tag.
    fn advance_inline(&mut self, ev: &Event, s: &str, end_pos: usize, doc: &mut DocMut) {
        let Some((start, depth)) = self.inline.as_mut() else {
            return;
        };
        match ev {
            Event::Start(_) => *depth = depth.saturating_add(1),
            Event::End(_) => {
                *depth = depth.saturating_sub(1);
                if *depth == 0 {
                    let raw = s.get(*start..end_pos).unwrap_or_default().to_string();
                    self.inline = None;
                    self.push_atom(doc, raw);
                }
            }
            _ => {}
        }
    }

    fn flush_para_marks(&mut self) {
        if let Some(b) = self.cur.as_ref() {
            for (name, value) in self.cur_para_marks.drain(..) {
                self.pending_marks
                    .push((b.clone(), 0, self.cur_len, name, value));
            }
        }
        self.cur_para_marks.clear();
    }

    /// Block-opening and stack-pushing events: notes/annotations/tracked-changes captures,
    /// headings, paragraphs, lists, tables, spans, links, and image frames.
    // A single-pass SAX dispatch over ~15 office:text element types
    // (paragraph/heading/list/table/span/link nesting, note/comment/
    // bookmark/change range tracking); the five self-contained
    // in-progress captures (fidelity/frame/note/annotation/tracked-changes)
    // are already split into the advance_* functions above — the rest
    // shares this whole struct's state across every arm, so further
    // extraction would need either a large per-event-kind helper
    // signature or a lifetime-entangled parser-state struct, neither
    // safe to introduce without compiling.
    #[allow(clippy::too_many_lines, reason = "see comment above the function")]
    fn handle_start(
        &mut self,
        e: &BytesStart,
        doc: &mut DocMut,
        reg: &StyleRegistry,
        pos_before: usize,
    ) {
        if self.is_unmodeled_para_child(e.name().as_ref()) {
            self.ensure_continuation(doc);
            self.inline = Some((pos_before, 1));
            return;
        }
        if self.in_para && matches!(e.name().as_ref(), b"text:note" | b"office:annotation") {
            self.ensure_continuation(doc);
        }
        if self.in_text && matches!(e.name().as_ref(), b"text:p" | b"text:h") {
            self.in_para = true;
            self.continue_para = false;
        }
        match e.name().as_ref() {
            b"text:note" if self.in_text && self.cur.is_some() => {
                // A footnote/endnote reference: splice the placeholder char now (so later text in this
                // paragraph lands after it), capture the element verbatim, and defer the `note-ref` mark until all text is in place.
                let idx = self.cur_len;
                append(
                    doc,
                    self.cur.as_deref(),
                    &mut self.cur_len,
                    &FIELD_PLACEHOLDER.to_string(),
                );
                self.note = Some((pos_before, 1, self.cur.clone(), idx));
            }
            b"office:annotation" if self.in_text && self.cur.is_some() => {
                // A comment anchored to a range: the annotation sits at the *start* of the commented text
                // (no placeholder). Capture the element verbatim and remember the current char index; the matching `annotation-end` records the range end.
                let idx = self.cur_len;
                self.annotation = Some((pos_before, 1, self.cur.clone(), idx));
            }
            b"text:tracked-changes" if self.in_text => {
                self.tracked = Some((pos_before, 1));
            }
            b"office:text" => self.in_text = true,
            b"text:h" if self.in_text => {
                let style = attr_val(e, b"text:style-name");
                let lvl = outline_level(e)
                    .or_else(|| {
                        style
                            .as_deref()
                            .and_then(|s| reg.table.heading_level(s, |_| None))
                    })
                    .unwrap_or(1)
                    .clamp(1, 6);
                let id = start_block(
                    doc,
                    &mut self.prev,
                    &mut self.cur,
                    &mut self.cur_len,
                    "heading",
                );
                doc.set_attr(&id, "level", lvl);
                if let Some(a) = style.as_deref().and_then(|s| reg.table.para_align(s)) {
                    doc.set_attr(&id, "align", a.as_str());
                }
                self.cur_para_marks.clear();
            }
            b"text:p" if self.in_text && !self.in_item && self.table_id.is_none() => {
                let style = attr_val(e, b"text:style-name");
                self.cur_para_marks = open_para(
                    doc,
                    &mut self.prev,
                    &mut self.cur,
                    &mut self.cur_len,
                    reg,
                    style.as_deref(),
                );
            }
            b"text:list" if self.in_text => {
                // A nested `text:list` often omits its own style-name, continuing the parent list's style at the deeper level — so inherit the parent's ordered-ness instead of defaulting to bulleted.
                let style = attr_val(e, b"text:style-name");
                if self.list_stack.is_empty() {
                    self.list_style = style.clone().filter(|s| !is_generated_style(s));
                }
                let ordered = style
                    .as_deref()
                    .and_then(|s| reg.list_ordered.get(s).copied())
                    .unwrap_or_else(|| *self.list_stack.last().unwrap_or(&false));
                self.list_stack.push(ordered);
                // Numbering fidelity: format + start from the list-style; the list's own `text:start-value`
                // overrides the style start, and `text:continue-numbering="true"` means it continues rather than restarting its count.
                let numbering = style.as_deref().and_then(|s| reg.list_numbering.get(s));
                let mut num_format = numbering.and_then(|n| n.num_format);
                let num_start = attr_val(e, b"text:start-value")
                    .and_then(|v| v.parse::<i64>().ok())
                    .or_else(|| numbering.and_then(|n| n.num_start));
                let continuing = attr_val(e, b"text:continue-numbering")
                    .is_some_and(|v| v == "true" || v == "1");
                // A list that doesn't continue restarts; an explicit start-value also implies a restart at that value.
                let restart = !continuing || num_start.is_some();
                if ordered && num_format.is_none() {
                    num_format = Some("decimal");
                }
                self.list_num_stack.push((num_format, num_start, restart));
            }
            b"text:list-item" if self.in_text => {
                self.item_paras = 0;
                let id = start_block(
                    doc,
                    &mut self.prev,
                    &mut self.cur,
                    &mut self.cur_len,
                    "list_item",
                );
                let ordered = *self.list_stack.last().unwrap_or(&false);
                doc.set_attr(&id, "ordered", ordered);
                if let Some(s) = self.list_style.as_deref() {
                    doc.set_attr(&id, ODT_LIST_STYLE, s);
                }
                // 0-based nesting depth. Match the markdown codec: only emit `level` when nested, so a top-level item's attr set stays identical across codecs.
                let level = i64::try_from(self.list_stack.len())
                    .unwrap_or(i64::MAX)
                    .saturating_sub(1)
                    .max(0);
                if level > 0 {
                    doc.set_attr(&id, "level", level);
                }
                // Numbering fidelity preservation (ordered lists only): carry format + start + restart so
                // the list re-exports with its numbering intact, keeping a plain decimal-from-1 list's attr set minimal.
                if ordered && let Some((fmt, start, restart)) = self.list_num_stack.last().copied()
                {
                    if let Some(f) = fmt
                        && f != "decimal"
                    {
                        doc.set_attr(&id, "num_format", f);
                    }
                    if let Some(s) = start
                        && s != 1
                    {
                        doc.set_attr(&id, "num_start", s);
                        if restart {
                            doc.set_attr(&id, "num_restart", true);
                        }
                    }
                }
                self.in_item = true;
            }
            b"text:p" if self.in_text && self.in_item => {
                self.join_item_paragraph(doc);
                if let Some(id) = self.cur.clone()
                    && let Some(a) = attr_val(e, b"text:style-name")
                        .as_deref()
                        .and_then(|s| reg.table.para_align(s))
                {
                    doc.set_attr(&id, "align", a.as_str());
                }
            }
            b"text:p" if self.in_text && self.table_id.is_some() => {
                // The paragraph inside a table cell: lift its alignment onto the cell block so centred/right-aligned cells round-trip.
                self.join_item_paragraph(doc);
                if let Some(id) = self.cur.clone()
                    && let Some(a) = attr_val(e, b"text:style-name")
                        .as_deref()
                        .and_then(|s| reg.table.para_align(s))
                {
                    doc.set_attr(&id, "align", a.as_str());
                }
            }
            b"table:table" if self.in_text => {
                self.table_id = Some(attr_val(e, b"table:name").unwrap_or_else(|| {
                    self.table_seq = self.table_seq.saturating_add(1);
                    format!("t{}", self.table_seq)
                }));
                self.trow = 0;
            }
            b"table:table-row" if self.in_text => self.tcol = 0,
            b"table:table-cell" if self.in_text => {
                self.item_paras = 0;
                self.open_cell(e, doc);
            }
            b"table:covered-table-cell" if self.in_text => self.cell_repeat = columns_repeated(e),
            b"text:span" if self.in_text => {
                let style = attr_val(e, b"text:style-name");
                let mut marks = style
                    .as_deref()
                    .map(|s| reg.table.text_marks(s))
                    .unwrap_or_default();
                // A style that says more than its marks (a character style, unmodeled text
                // properties) is referenced again on export.
                if let Some(s) = style.filter(|s| reg.text_extra.contains(s)) {
                    marks.push((MARK_ODT_SPAN.to_string(), MarkValue::Str(s)));
                }
                self.span_stack.push((marks, self.cur_len));
            }
            b"text:a" if self.in_text => {
                // A hyperlink: model the href as an inline `link` mark over the linked range (so it merges with concurrent text edits), plus any text marks the link's own style carries.
                let href = attr_val(e, b"xlink:href");
                let marks = attr_val(e, b"text:style-name")
                    .map(|s| reg.table.text_marks(&s))
                    .unwrap_or_default();
                self.link_stack.push((href, marks, self.cur_len));
            }
            b"draw:frame" if self.in_text && self.table_id.is_none() => {
                // An image frame — top-level or nested inside a `text:p` (the common LibreOffice/pandoc
                // figure layout). Capture its size now, resolve the picture bytes/alt as we descend, and
                // finalize on close into an `image` block; a frame inside a paragraph records that paragraph so an empty image-only wrapper can be dropped.
                if self.in_para {
                    self.ensure_continuation(doc);
                }
                let (enclosing_para, para_had_text) = if self.cur.is_some() && !self.in_item {
                    (self.cur.clone(), self.cur_len > 0)
                } else {
                    (None, false)
                };
                // `text:anchor-type`: `as-char` → inline; `page` → page; everything else (`paragraph`/`char`/`frame`) → paragraph-anchored floating.
                let anchor = attr_val(e, b"text:anchor-type").map(|a| match a.as_str() {
                    "as-char" => "inline".to_string(),
                    "page" => "page".to_string(),
                    _ => "paragraph".to_string(),
                });
                let wrap =
                    attr_val(e, b"draw:style-name").and_then(|s| reg.graphic_wrap.get(&s).cloned());
                self.frame = Some(FrameState {
                    raw_start: pos_before,
                    depth: 1,
                    width: attr_val(e, b"svg:width"),
                    height: attr_val(e, b"svg:height"),
                    anchor,
                    wrap,
                    href: None,
                    title: String::new(),
                    desc: String::new(),
                    collecting: None,
                    enclosing_para,
                    para_had_text,
                });
            }
            _ if self.in_text && self.cur.is_none() && !self.in_item && self.table_id.is_none() => {
                self.capture = Some((pos_before, 1));
            }
            _ => {}
        }
        self.keep_styles(e, doc, reg);
    }

    /// Keep what the model does not take from a style reference: a paragraph's automatic style
    /// (its first paragraph's, for a list item or cell) and a table's, column's, row's and
    /// cell's styles ride on the block (see `ODT_STYLE` and friends); export references them
    /// again while the modeled attributes still agree with them.
    fn keep_styles(&mut self, e: &BytesStart, doc: &mut DocMut, reg: &StyleRegistry) {
        if !self.in_text {
            return;
        }
        let style = attr_val(e, b"text:style-name").or_else(|| attr_val(e, b"table:style-name"));
        match e.name().as_ref() {
            b"text:p" | b"text:h" => {
                let first = !(self.in_item || self.table_id.is_some()) || self.item_paras == 1;
                if first
                    && let (Some(s), Some(id)) = (style, self.cur.as_deref())
                    && reg.table.para.get(&s).is_some_and(|p| !p.named)
                    && !is_generated_style(&s)
                {
                    doc.set_attr(id, ODT_STYLE, s.as_str());
                }
            }
            b"table:table" => self.table_style = style,
            b"table:table-row" => self.row_style = style,
            b"table:table-cell" => {
                if let Some(id) = self.cur.as_deref() {
                    for (key, value) in [
                        (ODT_TABLE_STYLE, self.table_style.take()),
                        (ODT_COLUMNS, self.table_columns.take()),
                        (ODT_ROW_STYLE, self.row_style.take()),
                        (ODT_CELL_STYLE, style),
                    ] {
                        if let Some(value) = value {
                            doc.set_attr(id, key, value.as_str());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Self-closing property/marker events: paragraph and heading shorthand, comment/bookmark/
    /// tracked-change range markers, tabs/breaks/spaces, table cells, and the fidelity-atom
    /// capture for a single unmodeled empty element.
    // See the comment on `handle_start` above — same shared-state dispatch shape.
    #[allow(clippy::too_many_lines, reason = "see comment above handle_start")]
    fn handle_empty(
        &mut self,
        e: &BytesStart,
        doc: &mut DocMut,
        reg: &StyleRegistry,
        s: &str,
        pos_before: usize,
        pos_after: usize,
    ) {
        if self.in_text && self.table_id.is_some() && e.name().as_ref() == b"table:table-column" {
            let raw = s.get(pos_before..pos_after).unwrap_or_default();
            self.table_columns
                .get_or_insert_with(String::new)
                .push_str(raw);
        }
        if self.is_unmodeled_para_child(e.name().as_ref())
            || (self.in_para && e.name().as_ref() == b"draw:frame")
        {
            let raw = s.get(pos_before..pos_after).unwrap_or_default().to_string();
            self.push_atom(doc, raw);
            return;
        }
        if self.in_para {
            self.ensure_continuation(doc);
        }
        match e.name().as_ref() {
            b"text:p" if self.in_text && !self.in_item && self.table_id.is_none() => {
                open_para(
                    doc,
                    &mut self.prev,
                    &mut self.cur,
                    &mut self.cur_len,
                    reg,
                    attr_val(e, b"text:style-name").as_deref(),
                );
                self.keep_styles(e, doc, reg);
                self.cur = None;
            }
            b"text:h" if self.in_text => {
                let style = attr_val(e, b"text:style-name");
                let lvl = outline_level(e)
                    .or_else(|| {
                        style
                            .as_deref()
                            .and_then(|s| reg.table.heading_level(s, |_| None))
                    })
                    .unwrap_or(1)
                    .clamp(1, 6);
                let id = start_block(
                    doc,
                    &mut self.prev,
                    &mut self.cur,
                    &mut self.cur_len,
                    "heading",
                );
                doc.set_attr(&id, "level", lvl);
                self.cur = None;
            }
            b"office:annotation-end" if self.in_text => {
                // Closes a comment's range: match by `office:name` to the open annotation and anchor a `comment` mark over `[start, cur_len)`.
                if let Some(name) = attr_val(e, b"office:name")
                    && let Some((cid, b, start)) = self.open_comments.remove(&name)
                {
                    self.comment_ranges.push((b, start, self.cur_len, cid));
                }
            }
            b"text:bookmark-start" if self.in_text && self.cur.is_some() => {
                // Opens a bookmark's range: remember the current char index; the matching `text:bookmark-end` (by `text:name`) records the end.
                if let (Some(name), Some(b)) = (attr_val(e, b"text:name"), self.cur.clone()) {
                    self.open_bookmarks.insert(name, (b, self.cur_len));
                }
            }
            b"text:bookmark-end" if self.in_text => {
                // Closes a bookmark's range: anchor a `bookmark` mark valued with the name over `[start, cur_len)`.
                if let Some(name) = attr_val(e, b"text:name")
                    && let Some((b, start)) = self.open_bookmarks.remove(&name)
                {
                    self.bookmark_ranges.push((b, start, self.cur_len, name));
                }
            }
            b"text:bookmark" if self.in_text && self.cur.is_some() => {
                // A point bookmark (no range): anchor a zero-width range at the current char index — widened to one character on apply below.
                if let (Some(name), Some(b)) = (attr_val(e, b"text:name"), self.cur.clone()) {
                    self.bookmark_ranges
                        .push((b, self.cur_len, self.cur_len, name));
                }
            }
            b"text:change-start" if self.in_text && self.cur.is_some() => {
                // Opens a tracked-change range: remember the current char index; the matching `text:change-end` records the end.
                if let Some(id) = super::changes::marker_change_id(e)
                    && let (Some((cid, kind)), Some(b)) =
                        (self.change_decls.get(&id).cloned(), self.cur.clone())
                {
                    self.open_changes.insert(id, (cid, kind, b, self.cur_len));
                }
            }
            b"text:change-end" if self.in_text => {
                // Closes a tracked-change range: anchor an `ins`/`del` mark over `[start, cur_len)` for the matching change id.
                if let Some(id) = super::changes::marker_change_id(e)
                    && let Some((cid, kind, b, start)) = self.open_changes.remove(&id)
                {
                    self.change_ranges.push((b, start, self.cur_len, kind, cid));
                }
            }
            b"text:change" if self.in_text && self.cur.is_some() => {
                // A point change marker. A deletion's text comes back into the body here, under a
                // `del` mark (qdoc keeps deleted text, struck). A collapsed insertion anchors a
                // 1-char range if there's a character after it (widened on apply), else a
                // zero-width range that drops.
                if let Some(id) = super::changes::marker_change_id(e)
                    && let (Some((cid, kind)), Some(b)) =
                        (self.change_decls.get(&id).cloned(), self.cur.clone())
                {
                    let start = self.cur_len;
                    if kind == ChangeKind::Del
                        && let Some(text) = self.deleted_texts.remove(&id)
                    {
                        append(doc, Some(b.as_str()), &mut self.cur_len, &text);
                    }
                    self.change_ranges.push((b, start, self.cur_len, kind, cid));
                }
            }
            b"text:tab" if self.in_text => {
                append(doc, self.cur.as_deref(), &mut self.cur_len, "\t");
            }
            b"text:line-break" if self.in_text => {
                append(doc, self.cur.as_deref(), &mut self.cur_len, "\n");
            }
            b"text:s" if self.in_text => {
                let n = attr_val(e, b"text:c")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1)
                    .min(256);
                append(doc, self.cur.as_deref(), &mut self.cur_len, &" ".repeat(n));
            }
            b"table:table-cell" if self.in_text => {
                self.open_cell(e, doc);
                self.keep_styles(e, doc, reg);
                self.cur = None;
                self.tcol = self.tcol.saturating_add(self.cell_repeat);
            }
            // A cell covered by a spanning neighbour holds a grid position but no content.
            b"table:covered-table-cell" if self.in_text => {
                self.tcol = self.tcol.saturating_add(columns_repeated(e));
            }
            _ if self.in_text && self.cur.is_none() && !self.in_item && self.table_id.is_none() => {
                let raw = s.get(pos_before..pos_after).unwrap_or_default();
                fidelity_block(doc, &mut self.prev, &mut self.cur, &mut self.cur_len, raw);
                self.cur = None;
            }
            _ => {}
        }
    }

    /// Closing events: property scope ends, and the span/link/list/table/paragraph closes that
    /// anchor their deferred marks.
    fn handle_end(&mut self, e: &quick_xml::events::BytesEnd) {
        if matches!(e.name().as_ref(), b"text:p" | b"text:h") {
            self.in_para = false;
            self.continue_para = false;
        }
        match e.name().as_ref() {
            b"office:text" => self.in_text = false,
            b"text:h" => {
                self.cur = None;
            }
            b"text:p" if !self.in_item && self.table_id.is_none() => {
                self.flush_para_marks();
                self.cur = None;
            }
            b"text:list" => {
                self.list_stack.pop();
                self.list_num_stack.pop();
            }
            b"table:table" => {
                self.table_id = None;
                self.cur = None;
            }
            b"table:table-row" => self.trow = self.trow.saturating_add(1),
            b"table:table-cell" | b"table:covered-table-cell" => {
                self.cur = None;
                self.tcol = self.tcol.saturating_add(self.cell_repeat);
            }
            b"text:list-item" => {
                self.flush_para_marks();
                self.cur = None;
                self.in_item = false;
            }
            b"text:span" if self.in_text => {
                if let Some((marks, start)) = self.span_stack.pop()
                    && let Some(b) = self.cur.as_ref()
                {
                    for (name, value) in marks {
                        self.pending_marks
                            .push((b.clone(), start, self.cur_len, name, value));
                    }
                }
            }
            b"text:a" if self.in_text => {
                if let Some((href, marks, start)) = self.link_stack.pop()
                    && let Some(b) = self.cur.as_ref()
                {
                    if let Some(h) = href {
                        self.pending_marks.push((
                            b.clone(),
                            start,
                            self.cur_len,
                            "link".to_string(),
                            MarkValue::Str(h),
                        ));
                    }
                    for (name, value) in marks {
                        self.pending_marks
                            .push((b.clone(), start, self.cur_len, name, value));
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_text(
        &mut self,
        t: &quick_xml::events::BytesText,
        doc: &mut DocMut,
    ) -> Result<(), CodecError> {
        if self.continue_para {
            let txt = t
                .xml10_content()
                .map_err(|e| CodecError::Parse(format!("text: {e}")))?;
            if !txt.is_empty() && !is_layout_whitespace(&txt) {
                self.ensure_continuation(doc);
            }
        }
        if let Some(b) = self.cur.as_ref() {
            let txt = t
                .xml10_content()
                .map_err(|e| CodecError::Parse(format!("text: {e}")))?;
            // Skip pretty-printer indentation between structural elements — otherwise list items / headings import with blank leading/trailing lines and huge vertical gaps.
            if !txt.is_empty() && !is_layout_whitespace(&txt) {
                doc.splice_text(b, self.cur_len, 0, &txt);
                self.cur_len = self.cur_len.saturating_add(txt.chars().count());
            }
        }
        Ok(())
    }

    /// An entity or character reference in paragraph text is the character it stands for.
    fn handle_general_ref(&mut self, r: &quick_xml::events::BytesRef, doc: &mut DocMut) {
        if let Some(b) = self.cur.as_ref()
            && let Some(c) = crate::resolve_general_ref(r)
        {
            doc.splice_text(b, self.cur_len, 0, c.encode_utf8(&mut [0; 4]));
            self.cur_len = self.cur_len.saturating_add(1);
        }
    }
}

/// What of `content.xml` surrounds the body: the root start tag, the declarations before the
/// automatic styles (`office:scripts`, `office:font-face-decls`, …) and the automatic styles'
/// content. Unmodeled body content kept verbatim refers to these (by namespace prefix, font
/// and style name), so export writes them back around the regenerated body.
pub(crate) struct ContentHead {
    pub root: String,
    pub prelude: String,
    pub automatic_styles: String,
}

pub(crate) fn content_head(content_xml: &[u8]) -> Option<ContentHead> {
    let s = std::str::from_utf8(content_xml).ok()?;
    let mut reader = Reader::from_str(s);
    let mut depth = 0usize;
    let mut root = None;
    let mut prelude = String::new();
    let mut automatic_styles = String::new();
    // Start of the root child being read and whether it is the automatic styles.
    let mut child: Option<(usize, bool)> = None;
    loop {
        let before = usize::try_from(reader.buffer_position()).ok()?;
        let ev = reader.read_event().ok()?;
        let after = usize::try_from(reader.buffer_position()).ok()?;
        match ev {
            Event::Start(e) => {
                match depth {
                    0 => root = Some(s.get(before..after)?.to_string()),
                    1 if e.name().as_ref() == b"office:body" => break,
                    1 => child = Some((before, e.name().as_ref() == b"office:automatic-styles")),
                    _ => {}
                }
                depth = depth.saturating_add(1);
            }
            Event::Empty(e) if depth == 1 && e.name().as_ref() != b"office:automatic-styles" => {
                prelude.push_str(s.get(before..after)?);
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 1
                    && let Some((start, is_auto)) = child.take()
                {
                    let raw = s.get(start..after)?;
                    if is_auto {
                        let open = raw.find('>')?.saturating_add(1);
                        let close = raw.len().saturating_sub("</office:automatic-styles>".len());
                        automatic_styles.push_str(raw.get(open..close).unwrap_or_default());
                    } else {
                        prelude.push_str(raw);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Some(ContentHead {
        root: root?,
        prelude,
        automatic_styles,
    })
}

pub(crate) fn parse_body(
    content_xml: &[u8],
    reg: &StyleRegistry,
    container: &ZipPackage,
    doc: &mut DocMut,
    inputs: &mut ContentInputs,
) -> Result<(), CodecError> {
    let s = std::str::from_utf8(content_xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);
    let mut st = BodyState::default();

    loop {
        let pos_before = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let ev = reader
            .read_event()
            .map_err(|e| CodecError::Parse(format!("xml: {e}")))?;
        let pos_after = || usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);

        if st.capture.is_some() {
            st.advance_capture(&ev, s, pos_after(), doc);
            continue;
        }

        // Inside a draw:frame: collect the image href + alt, then on close emit either an `image` block (if the Pictures part exists) or a verbatim fidelity atom.
        if st.frame.is_some() {
            match st.advance_frame(&ev, s, pos_after(), container, doc, inputs)? {
                FrameOutcome::Open => {}
                // A picture split its paragraph: what follows continues in a new block.
                FrameOutcome::Block => st.continue_para = st.in_para,
                FrameOutcome::Inline(raw) => st.push_atom(doc, raw),
            }
            continue;
        }

        if st.inline.is_some() {
            st.advance_inline(&ev, s, pos_after(), doc);
            continue;
        }

        // Inside an inline `text:note`: collect its raw XML until it closes, then parse it into a note + record a reference marker at the captured position.
        if st.note.is_some() {
            advance_note(&ev, s, pos_after(), &mut st.note, doc, &mut st.note_refs);
            continue;
        }

        // Inside an inline `office:annotation` (comment): collect its raw XML until it closes, then
        // parse it, remembering the start so the matching `annotation-end` (or, for a point comment, the same position) anchors a `comment` mark.
        if st.annotation.is_some() {
            advance_annotation(
                &ev,
                s,
                pos_after(),
                &mut st.annotation,
                doc,
                &mut st.open_comments,
                &mut st.comment_ranges,
            );
            continue;
        }

        // Inside the `text:tracked-changes` block: capture its raw XML until it closes, then parse the changed-region declarations into an `xml:id → (change id, kind)` map for body markers.
        if st.tracked.is_some() {
            advance_tracked(
                &ev,
                s,
                pos_after(),
                &mut st.tracked,
                doc,
                &mut st.change_decls,
                &mut st.deleted_texts,
            );
            continue;
        }

        match ev {
            Event::Start(e) => st.handle_start(&e, doc, reg, pos_before),
            Event::Empty(e) => st.handle_empty(&e, doc, reg, s, pos_before, pos_after()),
            Event::End(e) => st.handle_end(&e),
            Event::Text(t) => st.handle_text(&t, doc)?,
            Event::GeneralRef(r) => st.handle_general_ref(&r, doc),
            Event::Eof => break,
            _ => {}
        }
    }

    st.apply_deferred_marks(doc);
    Ok(())
}

impl BodyState {
    /// Apply every mark deferred during the parse: span/paragraph text marks, note-reference
    /// markers, and the comment/bookmark/tracked-change anchor ranges — closing any range whose
    /// explicit end never arrived by extending it to the block's end (or one character, if that's
    /// still zero-width and text follows).
    fn apply_deferred_marks(self, doc: &mut DocMut) {
        let Self {
            pending_marks,
            note_refs,
            mut open_comments,
            mut comment_ranges,
            mut open_bookmarks,
            mut bookmark_ranges,
            mut open_changes,
            mut change_ranges,
            ..
        } = self;
        // Apply marks now that all text is present (exact ranges, no expansion).
        for (b, start, end, name, value) in pending_marks {
            if start >= end {
                continue;
            }
            match value {
                MarkValue::Bool(v) => doc.add_mark(&b, start, end, &name, v),
                MarkValue::Int(n) => doc.add_mark(&b, start, end, &name, n),
                MarkValue::Str(s) => doc.add_mark(&b, start, end, &name, s.as_str()),
            }
        }
        // Note-reference markers: cover the placeholder spliced when each `text:note` opened, with a `note-ref` mark valued with the note id.
        for (b, idx, nid) in note_refs {
            doc.add_mark(&b, idx, idx.saturating_add(1), MARK_NOTE_REF, nid.as_str());
        }
        // Any annotation whose `office:annotation-end` never arrived (a point comment, or a malformed range) still anchors at its start, extending to the block's end when content follows.
        for (name, (cid, b, start)) in std::mem::take(&mut open_comments) {
            let _ = name;
            let end = doc_block_len(doc, &b).max(start);
            comment_ranges.push((b, start, end, cid));
        }
        // Comment anchor marks: cover the commented range with a `comment` mark valued with the comment id, widened to one character when zero-width and text follows.
        for (b, start, mut end, cid) in comment_ranges {
            if end <= start {
                let blen = doc_block_len(doc, &b);
                end = start.saturating_add(1).min(blen);
            }
            if end > start {
                doc.add_mark(&b, start, end, MARK_COMMENT, cid.as_str());
            }
        }
        // Any bookmark whose `text:bookmark-end` never arrived still anchors at its start, extending to the block's end when content follows.
        for (name, (b, start)) in std::mem::take(&mut open_bookmarks) {
            let end = doc_block_len(doc, &b).max(start);
            bookmark_ranges.push((b, start, end, name));
        }
        // Bookmark anchor marks: cover each bookmarked range with a `bookmark` mark valued with the
        // name, widened to one character when zero-width and text follows; an end-of-block point sits over the last character.
        for (b, start, end, name) in bookmark_ranges {
            doc.add_bookmark(&b, start, end, &name);
        }
        // Any tracked change whose `text:change-end` never arrived still anchors at its start, extending to the block's end when content follows.
        for (_id, (cid, kind, b, start)) in std::mem::take(&mut open_changes) {
            let end = doc_block_len(doc, &b).max(start);
            change_ranges.push((b, start, end, kind, cid));
        }
        // Tracked-change anchor marks: cover each changed range with the `ins`/`del` mark matching the change's kind, widened to one character when zero-width.
        for (b, start, mut end, kind, cid) in change_ranges {
            if end <= start {
                let blen = doc_block_len(doc, &b);
                end = start.saturating_add(1).min(blen);
            }
            if end > start {
                doc.add_change_mark(kind, &b, start, end, &cid);
            }
        }
    }
}

/// The character length of a body block's text (0 if absent).
fn doc_block_len(doc: &DocMut, block_id: &str) -> usize {
    doc.block_text_len(block_id)
}

#[cfg(test)]
mod tests {
    use super::super::OdtCodec;
    use super::super::container::{minimal_odt, minimal_odt_styled};
    use crate::FormatProvider;
    use tilo_doc::{AttrVal, Document};

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn imports_block_structure_lists_tables_and_fidelity_atoms() {
        // Phase `imports_headings_paragraphs_lists`.
        {
            let body = concat!(
                r#"<text:h text:outline-level="2">Title</text:h>"#,
                r#"<text:p>Hello</text:p>"#,
                r#"<text:list><text:list-item><text:p>one</text:p></text:list-item>"#,
                r#"<text:list-item><text:p>two</text:p></text:list-item></text:list>"#,
            );
            let odt = minimal_odt(body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();

            assert_eq!(snap.blocks[0].kind, "heading");
            assert_eq!(snap.blocks[0].text, "Title");
            assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(2)));
            assert_eq!(snap.blocks[1].kind, "paragraph");
            assert_eq!(snap.blocks[1].text, "Hello");
            assert_eq!(
                snap.blocks.iter().filter(|b| b.kind == "list_item").count(),
                2
            );
            assert_eq!(snap.blocks[2].text, "one");
            assert_eq!(snap.blocks[3].text, "two");
        }
        // Phase `imports_named_heading_style_whitespace_and_ordered_list`.
        {
            let styles = concat!(
                r#"<style:style style:name="Heading_20_1" style:display-name="Heading 1" style:family="paragraph" style:default-outline-level="1"/>"#,
                r#"<style:style style:name="MySub" style:family="paragraph" style:parent-style-name="Heading_20_1"/>"#,
                r#"<text:list-style style:name="L1"><text:list-level-style-number text:level="1"/></text:list-style>"#,
            );
            let body = concat!(
                r#"<text:p text:style-name="Heading_20_1">My Title</text:p>"#,
                r#"<text:p text:style-name="MySub">A Subtitle</text:p>"#,
                r#"<text:p>Line one<text:line-break/>line two<text:tab/>after a tab and<text:s text:c="3"/>spaces.</text:p>"#,
                r#"<text:list text:style-name="L1"><text:list-item><text:p>first</text:p></text:list-item><text:list-item><text:p>second</text:p></text:list-item></text:list>"#,
            );
            let odt = minimal_odt_styled(styles, body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();

            assert_eq!(
                snap.blocks[0].kind, "heading",
                "named paragraph style → heading: {snap:?}"
            );
            assert_eq!(snap.blocks[0].text, "My Title");
            assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(1)));
            assert_eq!(
                snap.blocks[1].kind, "heading",
                "heading via inherited parent style"
            );
            assert!(
                snap.blocks[2].text.contains('\n'),
                "line-break: {:?}",
                snap.blocks[2].text
            );
            assert!(
                snap.blocks[2].text.contains('\t'),
                "tab: {:?}",
                snap.blocks[2].text
            );
            assert!(
                snap.blocks[2].text.contains("and   spaces"),
                "text:s spaces: {:?}",
                snap.blocks[2].text
            );
            let items: Vec<_> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "list_item")
                .collect();
            assert_eq!(items.len(), 2);
            assert_eq!(
                items[0].attrs.get("ordered"),
                Some(&AttrVal::Bool(true)),
                "numbered list-style → ordered"
            );
        }
        // Phase `table_roundtrips_as_editable_cells`.
        {
            let body = concat!(
                r#"<table:table table:name="T1">"#,
                r#"<table:table-row><table:table-cell><text:p>a</text:p></table:table-cell>"#,
                r#"<table:table-cell><text:p>b</text:p></table:table-cell></table:table-row>"#,
                r#"<table:table-row><table:table-cell><text:p>c</text:p></table:table-cell>"#,
                r#"<table:table-cell><text:p>d</text:p></table:table-cell></table:table-row>"#,
                r#"</table:table>"#,
            );
            let odt = minimal_odt_styled("", body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();
            let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
            assert_eq!(cells.len(), 4, "2x2 cells: {snap:?}");
            assert_eq!(
                cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(),
                vec!["a", "b", "c", "d"]
            );
            assert_eq!(
                cells[0].attrs.get("table"),
                Some(&AttrVal::Str("T1".into()))
            );
            assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
            assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));

            let exported = OdtCodec
                .export(d.as_ref(), &tilo_content::NoContent)
                .unwrap();
            let mut d2 = Document::new();
            OdtCodec
                .import(
                    &exported,
                    &mut d2.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let cells2: Vec<_> = d2
                .materialize()
                .blocks
                .iter()
                .filter(|b| b.kind == "cell")
                .map(|b| b.text.clone())
                .collect();
            assert_eq!(
                cells2,
                vec!["a", "b", "c", "d"],
                "table survives export+import"
            );
        }
        // Phase `cell_alignment_roundtrips`.
        {
            // A centred table cell: alignment lives on the inner paragraph in ODF.
            let styles = r#"<style:style style:name="Pc" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>"#;
            let body = concat!(
                r#"<table:table table:name="T1"><table:table-row>"#,
                r#"<table:table-cell><text:p text:style-name="Pc">mid</text:p></table:table-cell>"#,
                r#"<table:table-cell><text:p>plain</text:p></table:table-cell>"#,
                r#"</table:table-row></table:table>"#,
            );
            let odt = minimal_odt_styled(styles, body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let cells: Vec<_> = d
                .materialize()
                .blocks
                .into_iter()
                .filter(|b| b.kind == "cell")
                .collect();
            assert_eq!(cells.len(), 2);
            assert_eq!(
                cells[0].attrs.get("align"),
                Some(&AttrVal::Str("center".into())),
                "centred cell: {:?}",
                cells[0].attrs
            );
            assert_eq!(cells[1].attrs.get("align"), None, "plain cell has no align");

            let exported = OdtCodec
                .export(d.as_ref(), &tilo_content::NoContent)
                .unwrap();
            let mut d2 = Document::new();
            OdtCodec
                .import(
                    &exported,
                    &mut d2.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let aligns: Vec<_> = d2
                .materialize()
                .blocks
                .iter()
                .filter(|b| b.kind == "cell")
                .map(|b| b.attrs.get("align").cloned())
                .collect();
            assert_eq!(
                aligns,
                vec![Some(AttrVal::Str("center".into())), None],
                "cell alignment round-trips"
            );
        }
        // Phase `nested_list_items_capture_level`.
        {
            let body = concat!(
                r#"<text:list><text:list-item><text:p>a</text:p></text:list-item>"#,
                r#"<text:list-item><text:p>b</text:p>"#,
                r#"<text:list><text:list-item><text:p>nested</text:p></text:list-item></text:list>"#,
                r#"</text:list-item></text:list>"#,
            );
            let odt = minimal_odt(body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();
            let items: Vec<_> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "list_item")
                .collect();
            let level = |b: &tilo_doc::snapshot::BlockView| match b.attrs.get("level") {
                Some(AttrVal::Int(n)) => *n,
                _ => 0,
            };
            assert_eq!(
                items
                    .iter()
                    .map(|b| (b.text.clone(), level(b)))
                    .collect::<Vec<_>>(),
                vec![("a".into(), 0i64), ("b".into(), 0), ("nested".into(), 1)],
                "nesting depth captured: {items:?}"
            );
            assert_eq!(
                items[0].attrs.get("level"),
                None,
                "top-level item has no level attr"
            );
        }
        // Phase `list_items_import_tight_without_layout_whitespace`.
        {
            let body = "<text:list>\n  <text:list-item>\n    <text:p>a</text:p>\n  </text:list-item>\n  <text:list-item>\n    <text:p>b</text:p>\n  </text:list-item>\n</text:list>";
            let odt = minimal_odt(body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();
            let items: Vec<_> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "list_item")
                .map(|b| b.text.clone())
                .collect();
            assert_eq!(
                items,
                vec!["a", "b"],
                "no indentation whitespace in item text: {items:?}"
            );
        }
        // Phase `preserves_unmodeled_body_element_as_fidelity_atom`.
        {
            let body = concat!(
                r#"<text:p>before</text:p>"#,
                r#"<draw:frame draw:name="F"><draw:image xlink:href="pic.png"/></draw:frame>"#,
                r#"<text:p>after</text:p>"#,
            );
            let odt = minimal_odt_styled("", body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();

            assert_eq!(snap.blocks.len(), 3, "before / fidelity / after: {snap:?}");
            assert_eq!(snap.blocks[0].kind, "paragraph");
            assert_eq!(snap.blocks[0].text, "before");
            assert_eq!(snap.blocks[1].kind, "fidelity");
            assert_eq!(snap.blocks[2].text, "after");
            let raw1 = match snap.blocks[1].attrs.get("xml") {
                Some(AttrVal::Str(x)) => x.clone(),
                other => panic!("fidelity atom missing xml attr: {other:?}"),
            };
            assert!(
                raw1.starts_with("<draw:frame")
                    && raw1.contains("pic.png")
                    && raw1.ends_with("</draw:frame>"),
                "captured raw: {raw1}"
            );

            let exported = OdtCodec
                .export(d.as_ref(), &tilo_content::NoContent)
                .unwrap();
            let mut d2 = Document::new();
            OdtCodec
                .import(
                    &exported,
                    &mut d2.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap2 = d2.materialize();
            assert_eq!(snap2.blocks.len(), 3);
            let raw2 = match snap2.blocks[1].attrs.get("xml") {
                Some(AttrVal::Str(x)) => x.clone(),
                other => panic!("fidelity atom missing after round-trip: {other:?}"),
            };
            assert_eq!(raw1, raw2, "fidelity atom round-trips verbatim");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn imports_spans_links_and_colors_as_marks() {
        // Phase `imports_spans_as_marks`.
        {
            let styles = concat!(
                r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>"#,
                r#"<style:style style:name="T2" style:family="text"><style:text-properties fo:font-style="italic"/></style:style>"#,
            );
            let body = concat!(
                r#"<text:p>a <text:span text:style-name="T1">bold</text:span>"#,
                r#" and <text:span text:style-name="T2">em</text:span></text:p>"#,
            );
            let odt = minimal_odt_styled(styles, body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();
            let p = &snap.blocks[0];
            assert_eq!(p.text, "a bold and em");
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "strong" && m.start == 2 && m.end == 6),
                "strong over 'bold', got {:?}",
                p.marks
            );
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "em" && m.start == 11 && m.end == 13),
                "em over 'em', got {:?}",
                p.marks
            );
        }
        // Phase `imports_paragraph_align_and_inline_font`.
        {
            use tilo_doc::document::MarkValue;
            let styles = concat!(
                r#"<style:style style:name="P2" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>"#,
                r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-size="20pt" style:font-name="Liberation Serif"/></style:style>"#,
            );
            let body = r#"<text:p text:style-name="P2"><text:span text:style-name="T1">big</text:span></text:p>"#;
            let odt = minimal_odt_styled(styles, body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let b = &d.materialize().blocks[0];
            assert_eq!(b.text, "big");
            assert_eq!(
                b.attrs.get("align"),
                Some(&AttrVal::Str("center".into())),
                "{:?}",
                b.attrs
            );
            assert!(
                b.marks
                    .iter()
                    .any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20)),
                "{:?}",
                b.marks
            );
            assert!(
                b.marks
                    .iter()
                    .any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())),
                "{:?}",
                b.marks
            );
        }
        // Phase `imports_hyperlink_as_link_mark`.
        {
            use tilo_doc::document::MarkValue;
            // A `text:a` becomes an inline `link` mark carrying the href, so it merges with concurrent text edits instead of being atomized.
            let body =
                r#"<text:p>see <text:a xlink:href="https://tilo.so/x">here</text:a>!</text:p>"#;
            let odt = minimal_odt(body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let b = &d.materialize().blocks[0];
            assert_eq!(b.text, "see here!");
            let link = b
                .marks
                .iter()
                .find(|m| m.name == "link")
                .expect("link mark present");
            assert_eq!(
                (link.start, link.end),
                (4, 8),
                "link over 'here': {:?}",
                b.marks
            );
            assert_eq!(link.value, MarkValue::Str("https://tilo.so/x".into()));
        }
        // Phase `imports_link_wrapping_styled_span`.
        {
            use tilo_doc::document::MarkValue;
            let styles = r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>"#;
            let body = r#"<text:p><text:a xlink:href="http://x/"><text:span text:style-name="T1">bold</text:span></text:a></text:p>"#;
            let odt = minimal_odt_styled(styles, body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let b = &d.materialize().blocks[0];
            assert_eq!(b.text, "bold");
            assert!(
                b.marks
                    .iter()
                    .any(|m| m.name == "link" && m.value == MarkValue::Str("http://x/".into())),
                "{:?}",
                b.marks
            );
            assert!(b.marks.iter().any(|m| m.name == "strong"), "{:?}", b.marks);
        }
        // Phase `imports_text_color_as_mark`.
        {
            use tilo_doc::document::MarkValue;
            let styles = r##"<style:style style:name="C1" style:family="text"><style:text-properties fo:color="#FF0000"/></style:style>"##;
            let body = r#"<text:p>a <text:span text:style-name="C1">red</text:span></text:p>"#;
            let odt = minimal_odt_styled(styles, body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let b = &d.materialize().blocks[0];
            assert_eq!(b.text, "a red");
            assert!(
                b.marks.iter().any(|m| m.name == "color"
                    && m.start == 2
                    && m.end == 5
                    && m.value == MarkValue::Str("#ff0000".into())),
                "color mark normalized + ranged: {:?}",
                b.marks
            );
        }
    }

    /// A valid 1x1 truecolor PNG so the in-paragraph image resolves to a real `image` block (a missing picture would fall back to a fidelity atom).
    const TINY_PNG: [u8; 69] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xf7, 0x03, 0x41, 0x43, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn imports_inline_images_with_alt_text_and_surrounding_text() {
        // Phase `imports_image_nested_in_paragraph`.
        {
            use crate::package::ZipPackage;
            let body = concat!(
                r#"<text:p>before</text:p>"#,
                r#"<text:p text:style-name="Fig">"#,
                r#"<draw:frame svg:width="200pt" svg:height="110pt"><draw:image xlink:href="Pictures/0.png"/></draw:frame>"#,
                r#"</text:p>"#,
                r#"<text:p>after</text:p>"#,
            );
            let base = minimal_odt_styled("", body);
            let mut c = ZipPackage::read(&base).unwrap();
            c.set("Pictures/0.png", TINY_PNG.to_vec());
            let odt = c.write_odf().unwrap();

            let mut d = Document::new();
            let mut inputs = tilo_content::ContentInputs::new();
            OdtCodec.import(&odt, &mut d.as_mut(), &mut inputs).unwrap();
            let snap = d.materialize();

            assert_eq!(
                snap.blocks
                    .iter()
                    .map(|b| b.kind.as_str())
                    .collect::<Vec<_>>(),
                vec!["paragraph", "image", "paragraph"],
                "image-only wrapper paragraph dropped: {snap:?}"
            );
            assert_eq!(snap.blocks[0].text, "before");
            assert_eq!(snap.blocks[2].text, "after");
            let img = &snap.blocks[1];
            let hash = match img.attrs.get("blob") {
                Some(AttrVal::Str(h)) => h.clone(),
                other => panic!("image missing blob attr: {other:?}"),
            };
            assert_eq!(
                inputs.get(&hash),
                Some(&TINY_PNG[..]),
                "image bytes handed to the caller"
            );
            // svg:width="200pt" → 200*96/72 ≈ 267px.
            assert_eq!(
                img.attrs.get("width"),
                Some(&AttrVal::Int(267)),
                "{:?}",
                img.attrs
            );
        }
        // Phase `image_alt_text_keeps_entity_references`.
        {
            use crate::package::ZipPackage;
            let body = concat!(
                r#"<text:p><draw:frame><draw:image xlink:href="Pictures/0.png"/>"#,
                "<svg:title>R&amp;D &lt;chart&gt; &#x1F600;</svg:title></draw:frame></text:p>",
            );
            let mut c = ZipPackage::read(&minimal_odt_styled("", body)).unwrap();
            c.set("Pictures/0.png", TINY_PNG.to_vec());
            let mut d = Document::new();
            OdtCodec
                .import(
                    &c.write_odf().unwrap(),
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();
            let img = snap.blocks.iter().find(|b| b.kind == "image").unwrap();
            assert_eq!(
                img.attrs.get("alt"),
                Some(&AttrVal::Str("R&D <chart> \u{1F600}".into()))
            );
        }
        // Phase `keeps_paragraph_text_before_inline_image`.
        {
            use crate::package::ZipPackage;
            let body = "<text:p>see <draw:frame><draw:image xlink:href=\"Pictures/0.png\"/></draw:frame></text:p>";
            let base = minimal_odt_styled("", body);
            let mut c = ZipPackage::read(&base).unwrap();
            c.set("Pictures/0.png", TINY_PNG.to_vec());
            let odt = c.write_odf().unwrap();

            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();
            assert_eq!(snap.blocks[0].kind, "paragraph");
            assert_eq!(snap.blocks[0].text, "see ", "leading text kept: {snap:?}");
            assert_eq!(snap.blocks[1].kind, "image");
        }
    }

    #[test]
    fn imports_and_roundtrips_document_and_run_language() {
        // Phase `imports_run_language_as_mark`.
        {
            use tilo_doc::document::MarkValue;
            // A text style carrying `fo:language` + `fo:country` → a `lang` run mark with the reassembled BCP-47 tag.
            let styles = r#"<style:style style:name="Lf" style:family="text"><style:text-properties fo:language="fr" fo:country="FR"/></style:style>"#;
            let body = r#"<text:p>a <text:span text:style-name="Lf">bonjour</text:span></text:p>"#;
            let odt = minimal_odt_styled(styles, body);
            let mut d = Document::new();
            OdtCodec
                .import(
                    &odt,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let b = &d.materialize().blocks[0];
            assert_eq!(b.text, "a bonjour");
            assert!(
                b.marks.iter().any(|m| m.name == "lang"
                    && m.start == 2
                    && m.end == 9
                    && m.value == MarkValue::Str("fr-FR".into())),
                "lang mark reassembled from fo:language+fo:country, ranged: {:?}",
                b.marks
            );
        }
        // Phase `document_and_run_language_roundtrip`.
        {
            use tilo_doc::document::MarkValue;
            // Author a document with a `de-DE` default language and a `fr`-marked run, export to `.odt`, re-import: both survive with BCP-47 reassembled from `fo:language`+`fo:country`.
            let mut d = Document::new();
            d.set_language("de-DE");
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "bonjour monde");
            d.add_mark(&id, 0, 7, "lang", "fr");

            let exported = OdtCodec
                .export(d.as_ref(), &tilo_content::NoContent)
                .unwrap();
            let mut d2 = Document::new();
            OdtCodec
                .import(
                    &exported,
                    &mut d2.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d2.materialize();

            assert_eq!(
                snap.language.as_deref(),
                Some("de-DE"),
                "document default language survives: {:?}",
                snap.language
            );
            let b = &snap.blocks[0];
            assert_eq!(b.text, "bonjour monde");
            assert!(
                b.marks.iter().any(|m| m.name == "lang"
                    && m.start == 0
                    && m.end == 7
                    && m.value == MarkValue::Str("fr".into())),
                "run lang mark `fr` survives over 'bonjour': {:?}",
                b.marks
            );
        }
    }

    #[test]
    fn keeps_unmodeled_paragraph_content_inline_through_export() {
        use crate::package::ZipPackage;
        let field = r#"<text:page-number text:select-page="current">3</text:page-number>"#;
        let shape = r#"<draw:custom-shape text:anchor-type="paragraph" svg:width="1cm" svg:height="1cm"><draw:enhanced-geometry draw:type="star5"/></draw:custom-shape>"#;
        let text_box = r#"<draw:frame text:anchor-type="paragraph" svg:width="3cm"><draw:text-box><text:p>boxed</text:p></draw:text-box></draw:frame>"#;
        let body = format!(
            r#"<text:p>{shape}Page {field} of<text:s text:c="2"/>two{text_box} after<text:tab/>tab</text:p>"#
        );
        let mut d = Document::new();
        OdtCodec
            .import(
                &minimal_odt(&body),
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        let snap = d.materialize();
        let ph = tilo_doc::schema::FIELD_PLACEHOLDER;
        assert_eq!(snap.blocks.len(), 1, "{snap:?}");
        let p = &snap.blocks[0];
        // The field shows its text; the shape and the text box show none.
        assert_eq!(p.text, format!("{ph}Page 3 of  two{ph} after\ttab"));
        assert_eq!(
            p.marks
                .iter()
                .filter(|m| m.name == crate::preserve::MARK_ODT_XML)
                .count(),
            3
        );

        let id = p.id.clone();
        d.splice_text(&id, p.text.chars().count(), 0, "!");
        let out = OdtCodec
            .export(d.as_ref(), &tilo_content::NoContent)
            .unwrap();
        let c = ZipPackage::read(&out).unwrap();
        let xml = String::from_utf8(c.get("content.xml").unwrap().to_vec()).unwrap();
        let at = |needle: &str| {
            xml.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {xml}"))
        };
        assert!(at(shape) < at("Page ") && at("Page ") < at(field) && at(field) < at(text_box));
        // Runs of spaces and tabs are written so ODF whitespace handling keeps them.
        assert!(xml.contains("of <text:s/>two"), "{xml}");
        assert!(xml.contains("after<text:tab/>tab!"), "{xml}");
        let mut again = Document::new();
        OdtCodec
            .import(
                &out,
                &mut again.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        assert_eq!(
            again.materialize().blocks[0].text,
            format!("{ph}Page 3 of  two{ph} after\ttab!")
        );

        // Text typed right after the field follows it; replacing the field's text writes the
        // new text instead of the field.
        let export_text = |d: &Document| {
            let out = OdtCodec
                .export(d.as_ref(), &tilo_content::NoContent)
                .unwrap();
            let c = ZipPackage::read(&out).unwrap();
            String::from_utf8(c.get("content.xml").unwrap().to_vec()).unwrap()
        };
        let after_field = "_Page 3".chars().count();
        d.splice_text(&id, after_field, 0, "x");
        let xml = export_text(&d);
        assert!(xml.contains(&format!("{field}x of")), "{xml}");
        d.splice_text(&id, after_field.saturating_sub(1), 2, "4");
        let xml = export_text(&d);
        assert!(
            !xml.contains("text:page-number") && xml.contains("Page 4 of"),
            "{xml}"
        );
    }
}
