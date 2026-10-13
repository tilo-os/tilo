//! Parse `word/document.xml`'s `w:body` into canonical blocks + marks, resolving
//! paragraph/heading/list/character styles against a pre-parsed `StyleRegistry`.
//! Paragraphs become `paragraph`/`heading`/`list_item` per `w:pStyle`/`w:numPr`; runs map
//! onto inline marks. Unmodeled body-level elements are captured verbatim as fidelity atoms.

use super::styles::{
    StyleRegistry, attr_val, line_spacing_from_spacing, map_jc, mark_from_rpr_child,
};
use crate::format::CodecError;
use crate::package::ZipPackage;
use crate::preserve::{ATTR_FORMAT, MARK_DOCX_XML};
use tilo_content::ContentInputs;
use tilo_doc::document::MarkValue;
use tilo_doc::model::DocMut;
use tilo_doc::schema::{FIELD_PLACEHOLDER, MARK_COMMENT, MARK_NOTE_REF};

use quick_xml::events::{BytesEnd, BytesRef, BytesStart, BytesText, Event};
use quick_xml::reader::Reader;
use std::collections::BTreeMap;

/// EMUs per CSS pixel (914400 EMU/inch ÷ 96 px/inch), for `wp:extent` → model pixel size.
const EMU_PER_PX: f64 = 914_400.0 / 96.0;

/// EMU → CSS pixel, rounded. Images are nowhere near i64's range, but a float has no checked
/// int conversion, so the bound is asserted with a clamp rather than assumed.
fn emu_to_px(emu: f64) -> i64 {
    let px = emu / EMU_PER_PX;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped to ±1_000_000_000 above, far inside i64 range for a pixel dimension"
    )]
    {
        px.round().clamp(-1_000_000_000.0, 1_000_000_000.0) as i64
    }
}

/// Build a `rId → media bytes` map from `word/_rels/document.xml.rels`, so the body parser can turn an `a:blip r:embed="rIdN"` into a stored image blob.
pub(crate) fn build_image_map(container: &ZipPackage) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let Some(rels) = container.get("word/_rels/document.xml.rels") else {
        return out;
    };
    let Ok(s) = std::str::from_utf8(rels) else {
        return out;
    };
    let mut reader = Reader::from_str(s);
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"Relationship" => {
                let id = attr_val(&e, b"Id");
                let target = attr_val(&e, b"Target");
                if let (Some(id), Some(target)) = (id, target) {
                    // Targets are relative to `word/`; absolute (`/word/...`) targets are de-slashed.
                    let t = target.trim_start_matches('/');
                    let part = if t.starts_with("word/") {
                        t.to_string()
                    } else {
                        format!("word/{t}")
                    };
                    if let Some(bytes) = container.get(&part) {
                        out.insert(id, bytes.to_vec());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// In-progress `w:drawing` capture: an embedded image with size + positioning.
#[derive(Default)]
struct DrawingState {
    /// `false` = `wp:inline` (inline), `true` = `wp:anchor` (floating).
    floating: bool,
    /// Display size in px from `wp:extent` (cx/cy in EMU).
    width: Option<i64>,
    height: Option<i64>,
    /// Model wrap (`none`|`left`|`right`|`both`) from the wrap element.
    wrap: Option<String>,
    /// Model anchor (`page` if positioned relative to the page, else `paragraph`).
    page_relative: bool,
    /// The `r:embed` rel id of the `a:blip` referencing the media part.
    rel: Option<String>,
}

/// The docx-id → qdoc-id lookup tables [`BodyState::handle_empty`] resolves footnote/endnote/
/// comment references against.
pub(crate) struct RefMaps<'a> {
    pub foot: &'a BTreeMap<String, String>,
    pub end: &'a BTreeMap<String, String>,
    pub comment: &'a BTreeMap<String, String>,
    pub links: &'a BTreeMap<String, String>,
}

/// Build an `rId → URL` map of the external hyperlink relationships in
/// `word/_rels/document.xml.rels`, which `w:hyperlink r:id` references.
pub(crate) fn build_link_map(container: &ZipPackage) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(rels) = container
        .get("word/_rels/document.xml.rels")
        .and_then(|b| std::str::from_utf8(b).ok())
    else {
        return out;
    };
    let mut reader = Reader::from_str(rels);
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"Relationship" => {
                let is_link = attr_val(&e, b"Type").is_some_and(|t| t.ends_with("/hyperlink"));
                if is_link
                    && attr_val(&e, b"TargetMode").as_deref() == Some("External")
                    && let (Some(id), Some(target)) = (attr_val(&e, b"Id"), attr_val(&e, b"Target"))
                {
                    out.insert(id, target);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// All per-parse mutable state for [`parse_body`]'s event loop, bundled so the loop can be split
/// into named handlers below instead of one 450-line function.
#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent XML-parse-state flag, set by name at its own open/close \
              tag handler, never positionally — there is no risk of two flags being swapped"
)]
struct BodyState {
    in_body: bool,
    prev: Option<String>,
    cur: Option<String>,
    cur_len: usize,

    // Paragraph-property scratch, gathered from `w:pPr` then applied at its close.
    in_ppr: bool,
    in_numpr: bool,
    p_style: Option<String>,
    p_align: Option<String>,
    p_numid: Option<String>,
    p_ilvl: i64,
    p_line_spacing: Option<f64>,

    // Run marks are collected for `w:rPr` and applied over the run's text range at `</w:r>`.
    in_run: bool,
    in_char_props: bool,
    in_t: bool,
    run_start: usize,
    run_marks: Vec<(String, MarkValue)>,

    pending_marks: Vec<(String, usize, usize, String, MarkValue)>,
    // Paragraph-style inline marks for the current block, applied over [0, len].
    cur_para_marks: Vec<(String, MarkValue)>,
    note_refs: Vec<(String, usize, String)>,
    // Open comment ranges keyed by docx id, awaiting their matching `w:commentRangeEnd`.
    open_comments: BTreeMap<String, (String, String, usize)>,
    comment_ranges: Vec<(String, usize, usize, String)>,

    // Open bookmark ranges keyed by the OOXML `w:id`, matched on `w:bookmarkEnd`; the name comes from `w:bookmarkStart`'s `w:name`.
    open_bookmarks: BTreeMap<String, (String, usize, String)>,
    bookmark_ranges: Vec<(String, usize, usize, String)>,

    // In-progress cross-reference (`w:hyperlink w:anchor="name"`): anchor target + start index; the `link` mark applies at close.
    hyperlink: Option<(String, Option<String>, usize)>,
    cross_refs: Vec<(String, usize, usize, String)>,

    // In-progress `<w:ins>`/`<w:del>` wrapper: change id, kind, anchor block, start index; the `ins`/`del` mark applies at close.
    // Deleted runs carry `<w:delText>` instead of `<w:t>` (treated identically here).
    change: Option<(
        String,
        tilo_doc::snapshot::ChangeKind,
        Option<String>,
        usize,
    )>,
    change_ranges: Vec<(String, usize, usize, tilo_doc::snapshot::ChangeKind, String)>,

    // Table state: cells become consecutive `cell` blocks tagged table/row/col.
    table_id: Option<String>,
    trow: usize,
    tcol: usize,
    table_seq: usize,

    capture: Option<(usize, i32)>, // (start byte, open depth) of a fidelity atom

    // In-progress `w:drawing`; becomes its own `image` block at `</w:drawing>`, replacing the enclosing empty paragraph.
    drawing: Option<DrawingState>,
    // Byte offset of the in-progress `w:drawing`, so a drawing that is not a resolvable image is kept verbatim.
    drawing_pos: usize,

    // Inside a `w:p` (body or table cell): unmodeled children become inline atoms.
    in_para: bool,
    // An image split the current paragraph: content after it continues in a new paragraph block.
    continue_para: bool,
    // Paragraphs opened so far in the current table cell; the second and later are joined by `\n`.
    cell_paras: usize,
    // Byte offset of the current `w:r`, and the verbatim `w:rPr` of that run (start offset while open).
    run_pos: usize,
    rpr_start: usize,
    rpr_xml: String,
    // In-progress verbatim capture of an unmodeled inline element or complex field.
    inline: Option<InlineCapture>,
    // (block, char index, verbatim XML) of each inline atom, marked once all text is in.
    atoms: Vec<(String, usize, usize, String)>,

    // The table cell being read, and the open vertical merges: grid column → (the merge's
    // first cell, rows it spans so far).
    cell: CellState,
    merges: BTreeMap<usize, (String, i64)>,

    // The current `w:pPr`'s unmodeled children, verbatim; element depth inside it and the
    // start of the direct child being read.
    ppr_kept: String,
    ppr_depth: usize,
    ppr_child: Option<(usize, Vec<u8>)>,
    // The current `w:pPr`'s `w:pStyle`, kept only when the model does not use the style.
    ppr_style: Option<String>,
    // (block, start, end, kept run properties) of runs with unmodeled run properties.
    rpr_marks: Vec<(String, usize, usize, String)>,

    // The current table's `w:tblPr`/`w:tblGrid` and row's `w:trPr`, until a cell takes them;
    // the start, name and depth of the table/row/cell properties element being read.
    table_pr: Option<String>,
    table_grid: Option<String>,
    row_pr: Option<String>,
    table_part: Option<(usize, Vec<u8>, usize)>,
}

/// What the body parser tracks about the table cell being read.
#[derive(Default)]
struct CellState {
    /// The cell's block, while it has one (a cell continuing a vertical merge has none).
    id: Option<String>,
    /// The block before the cell's own, to fall back to when the cell turns out to be covered.
    prev: Option<String>,
    /// Grid columns the cell spans (`w:gridSpan`).
    span: usize,
    /// The cell takes part in a vertical merge (`w:vMerge`).
    merged: bool,
}

/// List-item attr holding the imported `w:numId`, reused on export while the item still
/// numbers the same way.
pub(super) const ATTR_NUM: &str = "docx_num";

/// Paragraph children the body parser models; any other child of a `w:p` is kept verbatim as
/// an inline atom.
const PARA_MODELED: &[&[u8]] = &[
    b"w:pPr",
    b"w:r",
    b"w:ins",
    b"w:del",
    b"w:hyperlink",
    b"w:bookmarkStart",
    b"w:bookmarkEnd",
    b"w:commentRangeStart",
    b"w:commentRangeEnd",
    // Spell-check state: regenerated by the application, carrying nothing to keep.
    b"w:proofErr",
];

/// Run children the body parser models (`w:br` and `w:fldChar` are decided per element); any
/// other child of a `w:r` is kept verbatim, wrapped in a run with that run's properties, as an
/// inline atom.
const RUN_MODELED: &[&[u8]] = &[
    b"w:rPr",
    b"w:t",
    b"w:delText",
    b"w:tab",
    b"w:cr",
    b"w:footnoteReference",
    b"w:endnoteReference",
    b"w:drawing",
    // Regenerated from the comment's range on export.
    b"w:commentReference",
    // A layout cache of where the application last broke the page.
    b"w:lastRenderedPageBreak",
];

/// An in-progress verbatim capture of paragraph content the model does not represent.
///
/// A single element (a `w:sdt`, `m:oMath`, a run holding a `w:sym`, …) is captured from its
/// start tag to its end tag. A complex field (`w:fldChar` begin … end) is captured whole — its
/// instruction and its result — from the run holding `begin` to the run holding `end`. When it
/// crosses the end of an element opened before it (the paragraph, a `w:ins`, a hyperlink), the
/// part captured so far becomes one atom there and the capture resumes with the next paragraph
/// content, so each atom is well-formed on its own and the pieces re-emit in order.
struct InlineCapture {
    /// Start offset of the segment being captured; `None` between the segments of a field.
    start: Option<usize>,
    /// Element depth inside the segment.
    depth: i32,
    /// `<w:r>` + the run's properties, when the segment started inside a run; closed with `</w:r>`.
    prefix: Option<String>,
    /// Open complex fields (`begin` minus `end`).
    field: i32,
    /// The segment started at its run's start tag, so the run's end is consumed with it.
    owns_run: bool,
}

/// `begin`/`end` of a `w:fldChar` as a field-depth change.
fn field_delta(e: &BytesStart) -> i32 {
    match attr_val(e, b"w:fldCharType").as_deref() {
        Some("begin") => 1,
        Some("end") => -1,
        _ => 0,
    }
}

impl BodyState {
    /// After an image split the paragraph, open the block that carries the rest of its content.
    fn ensure_continuation(&mut self, doc: &mut DocMut) {
        if self.cur.is_none() && self.continue_para && self.in_para {
            self.continue_para = false;
            self.start_block(doc, "paragraph");
            self.run_start = 0;
        }
    }

    /// Record `raw` as an inline atom at the current position: its visible text (or one
    /// placeholder when it shows none, see `preserve::covered_text`), covered by a mark holding
    /// the element verbatim.
    fn push_atom(&mut self, doc: &mut DocMut, raw: String) {
        self.ensure_continuation(doc);
        if let Some(b) = self.cur.clone() {
            let idx = self.cur_len;
            self.append(doc, &crate::preserve::covered_text(&raw, MARK_DOCX_XML));
            self.atoms.push((b, idx, self.cur_len, raw));
        }
    }

    /// Whether an element starting here is paragraph content the model does not represent.
    fn is_unmodeled_para_child(&self, name: &[u8]) -> bool {
        self.in_para
            && !self.in_run
            && !self.in_ppr
            && self.drawing.is_none()
            && !PARA_MODELED.contains(&name)
    }

    /// Whether an element starting here is run content the model does not represent.
    fn is_unmodeled_run_child(&self, e: &BytesStart) -> bool {
        if !self.in_run || self.in_char_props || self.in_t || self.drawing.is_some() {
            return false;
        }
        match e.name().as_ref() {
            // A page or column break is layout, not a line break in the text.
            b"w:br" => attr_val(e, b"w:type").is_some_and(|t| t != "textWrapping"),
            name => !RUN_MODELED.contains(&name),
        }
    }

    /// Start keeping an unmodeled element verbatim: an empty element is an atom right away, a
    /// start tag opens a capture that [`Self::handle_inline`] closes at its end tag.
    fn begin_atom(&mut self, raw_tag: &str, pos_before: usize, is_start: bool, doc: &mut DocMut) {
        let prefix = self.in_run.then(|| format!("<w:r>{}", self.rpr_xml));
        if is_start {
            self.inline = Some(InlineCapture {
                start: Some(pos_before),
                depth: 1,
                prefix,
                field: 0,
                owns_run: false,
            });
        } else {
            let mut raw = prefix.clone().unwrap_or_default();
            raw.push_str(raw_tag);
            if prefix.is_some() {
                raw.push_str("</w:r>");
            }
            self.push_atom(doc, raw);
        }
    }

    /// A `w:fldChar begin` in a run: capture the whole field. Starting at the run's own start
    /// tag when nothing of the run has been read into the text yet keeps the run intact.
    fn begin_field(&mut self, pos_before: usize, is_start: bool) {
        let owns_run = self.cur_len == self.run_start;
        let inner = i32::from(is_start);
        self.inline = Some(if owns_run {
            InlineCapture {
                start: Some(self.run_pos),
                depth: inner.saturating_add(1),
                prefix: None,
                field: 1,
                owns_run: true,
            }
        } else {
            InlineCapture {
                start: Some(pos_before),
                depth: inner,
                prefix: Some(format!("<w:r>{}", self.rpr_xml)),
                field: 1,
                owns_run: false,
            }
        });
    }

    /// Close the current capture segment at byte `end` into an inline atom.
    fn flush_inline(&mut self, s: &str, end: usize, doc: &mut DocMut) {
        let Some(cap) = self.inline.as_mut() else {
            return;
        };
        let Some(start) = cap.start.take() else {
            return;
        };
        let prefix = cap.prefix.take();
        let owns_run = std::mem::take(&mut cap.owns_run);
        let mut raw = prefix.clone().unwrap_or_default();
        raw.push_str(s.get(start..end).unwrap_or_default());
        if prefix.is_some() {
            raw.push_str("</w:r>");
        }
        if owns_run {
            self.in_run = false;
            self.in_char_props = false;
            self.in_t = false;
            self.run_marks.clear();
        }
        self.push_atom(doc, raw);
    }

    /// While an inline capture is open, consume events into it. Returns `false` for events the
    /// regular handlers must see: everything outside a field's segments, and the end tag of an
    /// element opened before the segment (which closes the segment first).
    fn handle_inline(
        &mut self,
        s: &str,
        pos_before: usize,
        pos_after: usize,
        ev: &Event,
        doc: &mut DocMut,
    ) -> bool {
        let opens_segment = match ev {
            Event::Start(e) | Event::Empty(e) => {
                self.in_para && !self.in_ppr && !self.in_run && e.name().as_ref() != b"w:pPr"
            }
            _ => false,
        };
        let Some(cap) = self.inline.as_mut() else {
            return false;
        };
        if cap.start.is_none() {
            if !opens_segment {
                return false;
            }
            cap.start = Some(pos_before);
            cap.depth = 0;
        }
        match ev {
            Event::Start(e) => {
                cap.depth = cap.depth.saturating_add(1);
                if e.name().as_ref() == b"w:fldChar" {
                    cap.field = cap.field.saturating_add(field_delta(e)).max(0);
                }
            }
            Event::Empty(e) => {
                if e.name().as_ref() == b"w:fldChar" {
                    cap.field = cap.field.saturating_add(field_delta(e)).max(0);
                }
            }
            Event::End(_) => {
                if cap.depth == 0 {
                    let done = cap.field == 0;
                    self.flush_inline(s, pos_before, doc);
                    if done {
                        self.inline = None;
                    }
                    return false;
                }
                cap.depth = cap.depth.saturating_sub(1);
                if cap.depth == 0 && cap.field == 0 {
                    self.flush_inline(s, pos_after, doc);
                    self.inline = None;
                }
            }
            _ => {}
        }
        true
    }

    /// Keep an unmodeled element verbatim (or start capturing a complex field) when `e` is
    /// one; `true` when it was.
    fn begin_unmodeled(
        &mut self,
        e: &BytesStart,
        raw: &str,
        pos_before: usize,
        is_start: bool,
        doc: &mut DocMut,
    ) -> bool {
        if self.in_run
            && !self.in_char_props
            && e.name().as_ref() == b"w:fldChar"
            && field_delta(e) > 0
        {
            self.begin_field(pos_before, is_start);
            return true;
        }
        if self.is_unmodeled_run_child(e) || self.is_unmodeled_para_child(e.name().as_ref()) {
            self.begin_atom(raw, pos_before, is_start, doc);
            return true;
        }
        false
    }

    /// A `w:hyperlink`: remember its target + start index; the `link` mark applies at its end.
    /// An external link's `r:id` names a relationship holding the URL; a cross-reference
    /// (`w:anchor="name"`) points inside the document.
    fn open_hyperlink(&mut self, e: &BytesStart, maps: &RefMaps) {
        let target = match (attr_val(e, b"r:id"), attr_val(e, b"w:anchor")) {
            (Some(rid), _) if maps.links.contains_key(&rid) => maps.links.get(&rid).cloned(),
            (_, Some(anchor)) => Some(format!("{}{anchor}", tilo_doc::schema::CROSS_REF_PREFIX)),
            _ => None,
        };
        if let Some(target) = target {
            self.hyperlink = Some((target, self.cur.clone(), self.cur_len));
        }
    }

    /// A `w:gridSpan` in the current cell's properties: the cell spans that many grid columns.
    fn grid_span(&mut self, e: &BytesStart, doc: &mut DocMut) {
        let span = attr_val(e, b"w:val")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1)
            .clamp(1, 1024);
        self.cell.span = span;
        if let (Some(id), Ok(n)) = (self.cur.as_deref(), i64::try_from(span))
            && n > 1
        {
            doc.set_attr(id, "colspan", n);
        }
    }

    /// At a run's end: its unmodeled run properties ride on its text as a mark, joined with
    /// the previous run's when they are the same and adjacent.
    fn keep_run_props(&mut self) {
        let Some(b) = self.cur.as_ref() else {
            return;
        };
        if self.rpr_xml.is_empty() || self.run_start >= self.cur_len {
            return;
        }
        let kept = super::paragraph::kept_run_props(&self.rpr_xml);
        if kept.is_empty() {
            return;
        }
        if let Some(last) = self.rpr_marks.last_mut()
            && last.0 == *b
            && last.2 == self.run_start
            && last.3 == kept
        {
            last.2 = self.cur_len;
            return;
        }
        self.rpr_marks
            .push((b.clone(), self.run_start, self.cur_len, kept));
    }

    /// A `w:vMerge` in the current cell's properties: `restart` opens a vertical merge at this
    /// grid column; otherwise the cell continues the merge above it, so it adds a row to that
    /// merge's first cell and has no block of its own.
    fn vertical_merge(&mut self, e: &BytesStart, doc: &mut DocMut) {
        self.cell.merged = true;
        let Some(id) = self.cur.clone() else {
            return;
        };
        if attr_val(e, b"w:val").as_deref() == Some("restart") {
            self.merges.insert(self.tcol, (id, 1));
        } else if let Some((first, rows)) = self.merges.get_mut(&self.tcol) {
            *rows = rows.saturating_add(1);
            doc.set_attr(first, "rowspan", *rows);
            doc.remove_block(&id);
            self.cur = None;
            self.cell.id = None;
            self.prev = self.cell.prev.clone();
        }
    }

    /// Opens a fresh top-level block anchored after the previous one, and makes it current.
    fn start_block(&mut self, doc: &mut DocMut, kind: &str) -> String {
        let id = doc.insert_block(self.prev.as_deref(), kind);
        self.prev = Some(id.clone());
        self.cur = Some(id.clone());
        self.cur_len = 0;
        id
    }

    fn append(&mut self, doc: &mut DocMut, txt: &str) {
        self.ensure_continuation(doc);
        if let Some(b) = self.cur.as_deref()
            && !txt.is_empty()
        {
            doc.splice_text(b, self.cur_len, 0, txt);
            self.cur_len = self.cur_len.saturating_add(txt.chars().count());
        }
    }

    /// Flushes the current block's paragraph-style inline marks (set by [`Self::finalize_ppr`]) over `[0, cur_len)`.
    fn flush_para_marks(&mut self) {
        if let Some(b) = self.cur.as_ref() {
            for (name, value) in self.cur_para_marks.drain(..) {
                self.pending_marks
                    .push((b.clone(), 0, self.cur_len, name, value));
            }
        }
        self.cur_para_marks.clear();
    }

    /// Inside a table, collect the table's, rows' and cells' properties verbatim (see
    /// `paragraph::ATTR_TBLPR`). Only observes: the regular handlers still see every event.
    fn observe_table(
        &mut self,
        ev: &Event,
        s: &str,
        pos_before: usize,
        pos_after: usize,
        doc: &mut DocMut,
    ) {
        if self.table_id.is_none() || self.in_para {
            return;
        }
        let is_part = |e: &BytesStart| {
            matches!(
                e.name().as_ref(),
                b"w:tblPr" | b"w:tblGrid" | b"w:trPr" | b"w:tcPr"
            )
        };
        match ev {
            Event::Start(e) => match self.table_part.as_mut() {
                Some(part) => part.2 = part.2.saturating_add(1),
                None if is_part(e) => {
                    self.table_part = Some((pos_before, e.name().as_ref().to_vec(), 0));
                }
                None => {}
            },
            Event::Empty(e) if self.table_part.is_none() && is_part(e) => {
                let raw = s.get(pos_before..pos_after).unwrap_or_default();
                self.keep_table_part(e.name().as_ref(), raw, doc);
            }
            Event::End(_) => match self.table_part.take() {
                Some((start, name, 0)) => {
                    let raw = s.get(start..pos_after).unwrap_or_default();
                    self.keep_table_part(&name, raw, doc);
                }
                Some((start, name, depth)) => {
                    self.table_part = Some((start, name, depth.saturating_sub(1)));
                }
                None => {}
            },
            _ => {}
        }
    }

    fn keep_table_part(&mut self, name: &[u8], raw: &str, doc: &mut DocMut) {
        match name {
            b"w:tblPr" | b"w:tblGrid" if !super::paragraph::is_informative_table_part(raw) => {}
            b"w:tblPr" => self.table_pr = Some(raw.to_string()),
            b"w:tblGrid" => self.table_grid = Some(raw.to_string()),
            b"w:trPr" => self.row_pr = Some(raw.to_string()),
            _ => {
                let kept = super::paragraph::kept_cell_props(raw);
                if !kept.is_empty()
                    && let Some(id) = self.cell.id.as_deref()
                {
                    doc.set_attr(id, super::paragraph::ATTR_TCPR, kept.as_str());
                }
            }
        }
    }

    /// Inside a `w:pPr`, collect the direct children the model does not represent, verbatim.
    /// Only observes: the regular handlers still see every event.
    fn observe_ppr(&mut self, ev: &Event, s: &str, pos_before: usize, pos_after: usize) {
        if !self.in_ppr {
            return;
        }
        match ev {
            Event::Start(e) => {
                if self.ppr_depth == 0 {
                    self.ppr_child = Some((pos_before, e.name().as_ref().to_vec()));
                }
                self.ppr_depth = self.ppr_depth.saturating_add(1);
            }
            Event::Empty(e) if self.ppr_depth == 0 => {
                let raw = s.get(pos_before..pos_after).unwrap_or_default();
                if e.name().as_ref() == b"w:pStyle" {
                    self.ppr_style = Some(raw.to_string());
                } else if let Some(kept) =
                    super::paragraph::kept_child(e, raw, self.table_id.is_some())
                {
                    self.ppr_kept.push_str(&kept);
                }
            }
            Event::End(_) if self.ppr_depth > 0 => {
                self.ppr_depth = self.ppr_depth.saturating_sub(1);
                if self.ppr_depth == 0
                    && let Some((start, name)) = self.ppr_child.take()
                    && name != b"w:numPr"
                {
                    self.ppr_kept
                        .push_str(s.get(start..pos_after).unwrap_or_default());
                }
            }
            _ => {}
        }
    }

    /// Finalise the paragraph's properties: pick the block kind (`list_item`/heading/paragraph), set attrs, stash whole-block marks.
    fn finalize_ppr(&mut self, doc: &mut DocMut, reg: &StyleRegistry) {
        let Some(id) = self.cur.clone() else {
            return;
        };
        // The unmodeled properties ride on the block verbatim; the style reference only when
        // the model did not take it up (as a heading or lifted named style) — a list item's
        // always, as the model has no paragraph style for it.
        let mut kept = std::mem::take(&mut self.ppr_kept);
        let style = self.ppr_style.take();
        let used_style = self.p_numid.is_none()
            && self.p_style.as_deref().is_some_and(|s| {
                reg.heading_level(s).is_some()
                    || (reg.table.is_named_para(s)
                        && reg.table.style_props(s, str::to_string).is_some())
            });
        if let Some(style) = style.filter(|_| !used_style) {
            kept.insert_str(0, &style);
        }
        if !kept.is_empty() {
            doc.set_attr(&id, super::paragraph::ATTR_PPR, kept.as_str());
        }
        // Line spacing rides as `line_spacing` regardless of block kind; a direct `w:spacing` wins over inheriting from the style.
        let line_spacing = self.p_line_spacing.or_else(|| {
            self.p_style
                .as_deref()
                .and_then(|s| reg.table.para_line_height(s))
        });
        if let Some(ls) = line_spacing {
            doc.set_attr(&id, "line_spacing", ls);
        }
        let level = self.p_style.as_deref().and_then(|s| reg.heading_level(s));
        if let Some(numid) = self.p_numid.clone() {
            doc.set_kind(&id, "list_item");
            let ordered = reg.num_ordered.get(&numid).copied().unwrap_or(false);
            doc.set_attr(&id, "ordered", ordered);
            doc.set_attr(&id, ATTR_NUM, numid.as_str());
            // 0-based nesting depth, only when nested (as the other codecs do).
            if self.p_ilvl > 0 {
                doc.set_attr(&id, "level", self.p_ilvl);
            }
            // Numbering fidelity (ordered lists only): preserve format + start so the list re-exports intact.
            if ordered && let Some(def) = reg.num_def.get(&numid) {
                if let Some(f) = def.num_format
                    && f != "decimal"
                {
                    doc.set_attr(&id, "num_format", f);
                }
                if let Some(s) = def.num_start
                    && s != 1
                {
                    doc.set_attr(&id, "num_start", s);
                    // OOXML numbering restarts by default; a non-1 start implies a restart at that value.
                    doc.set_attr(&id, "num_restart", true);
                }
            }
            // Direct `w:jc` wins; otherwise inherit alignment from the style.
            let align = self.p_align.clone().or_else(|| {
                self.p_style
                    .as_deref()
                    .and_then(|s| reg.table.para_align(s))
            });
            if let Some(a) = align {
                doc.set_attr(&id, "align", a.as_str());
            }
        } else if let Some(l) = level {
            doc.set_kind(&id, "heading");
            doc.set_attr(&id, "level", l);
            let align = self.p_align.clone().or_else(|| {
                self.p_style
                    .as_deref()
                    .and_then(|s| reg.table.para_align(s))
            });
            if let Some(a) = align {
                doc.set_attr(&id, "align", a.as_str());
            }
        } else {
            // A plain paragraph with a *named* style lifts the style's definition into the styles container and points the block at
            // it via `style`, rather than also baking its formatting onto the block — which would break round-trip idempotency.
            let lifted = self
                .p_style
                .as_deref()
                .filter(|s| reg.table.is_named_para(s))
                .and_then(|s| {
                    reg.table
                        .style_props(s, str::to_string)
                        .map(|props| (s.to_string(), props))
                });
            if let Some((sid, props)) = lifted {
                doc.set_style(&sid, &props);
                doc.set_attr(&id, "style", sid.as_str());
                if let Some(a) = self.p_align.clone() {
                    doc.set_attr(&id, "align", a.as_str());
                }
            } else {
                let align = self.p_align.clone().or_else(|| {
                    self.p_style
                        .as_deref()
                        .and_then(|s| reg.table.para_align(s))
                });
                if let Some(a) = align {
                    doc.set_attr(&id, "align", a.as_str());
                }
                if let Some(st) = self.p_style.clone() {
                    self.cur_para_marks = reg.table.para_text_marks(&st);
                }
            }
        }
    }

    /// While a fidelity atom is being captured (`capture` is `Some`), tracks element depth and
    /// closes/records the atom at depth 0. Returns `true` when the event was consumed here.
    fn handle_capture(&mut self, s: &str, pos_after: usize, ev: &Event, doc: &mut DocMut) -> bool {
        let Some((start, depth)) = self.capture.as_mut() else {
            return false;
        };
        match ev {
            Event::Start(_) => *depth = depth.saturating_add(1),
            Event::End(_) => {
                *depth = depth.saturating_sub(1);
                if *depth == 0 {
                    let raw = s.get(*start..pos_after).unwrap_or_default().to_string();
                    let id = self.start_block(doc, "fidelity");
                    doc.set_attr(&id, "xml", raw.as_str());
                    doc.set_attr(&id, ATTR_FORMAT, "docx");
                    self.cur = None;
                    self.capture = None;
                }
            }
            _ => {}
        }
        true
    }

    /// Inside a `w:drawing`: collect inline-vs-anchor, extent, wrap, and the blip rel, then
    /// materialise on `</w:drawing>`. Returns `true` when the event was consumed here.
    fn handle_drawing(
        &mut self,
        ev: &Event,
        doc: &mut DocMut,
        image_map: &BTreeMap<String, Vec<u8>>,
        inputs: &mut ContentInputs,
        s: &str,
        pos_after: usize,
    ) -> bool {
        if self.drawing.is_none() {
            return false;
        }
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                if let Some(dr) = self.drawing.as_mut() {
                    match e.name().as_ref() {
                        b"wp:inline" => dr.floating = false,
                        b"wp:anchor" => dr.floating = true,
                        b"wp:extent" => {
                            if let Some(cx) = attr_val(e, b"cx").and_then(|v| v.parse::<f64>().ok())
                            {
                                dr.width = Some(emu_to_px(cx));
                            }
                            if let Some(cy) = attr_val(e, b"cy").and_then(|v| v.parse::<f64>().ok())
                            {
                                dr.height = Some(emu_to_px(cy));
                            }
                        }
                        // wrapNone/wrapTopAndBottom don't flow text beside the image → `none`; wrapSquare/wrapTight/wrapThrough use the wrapText side.
                        b"wp:wrapNone" | b"wp:wrapTopAndBottom" => {
                            dr.wrap = Some("none".to_string());
                        }
                        b"wp:wrapSquare" | b"wp:wrapTight" | b"wp:wrapThrough" => {
                            let side = attr_val(e, b"wrapText");
                            dr.wrap = Some(
                                match side.as_deref() {
                                    Some("left") => "left",
                                    Some("right") => "right",
                                    _ => "both", // "bothSides" | "largest" | absent
                                }
                                .to_string(),
                            );
                        }
                        // Page-relative horizontal/vertical positioning marks a page anchor.
                        b"wp:positionH" | b"wp:positionV" => {
                            if attr_val(e, b"relativeFrom").as_deref() == Some("page") {
                                dr.page_relative = true;
                            }
                        }
                        b"a:blip" if dr.rel.is_none() => {
                            dr.rel = attr_val(e, b"r:embed").or_else(|| attr_val(e, b"r:link"));
                        }
                        _ => {}
                    }
                }
            }
            Event::End(e) if e.name().as_ref() == b"w:drawing" => {
                let is_picture = self
                    .drawing
                    .as_ref()
                    .and_then(|dr| dr.rel.as_deref())
                    .is_some_and(|r| image_map.contains_key(r));
                if !is_picture {
                    // Not a picture the model can hold (a shape, chart, text box, linked
                    // image): keep the drawing verbatim, in its run, where it stood.
                    self.drawing = None;
                    let raw = s.get(self.drawing_pos..pos_after).unwrap_or_default();
                    let atom = format!("<w:r>{}{raw}</w:r>", self.rpr_xml);
                    self.push_atom(doc, atom);
                    return true;
                }
                if let Some(dr) = self.drawing.take()
                    && let Some(bytes) = dr.rel.as_deref().and_then(|r| image_map.get(r))
                {
                    // Reuse the enclosing (image-only) paragraph if empty, else open a fresh block, as `image`.
                    let id = if let (Some(c), 0) = (self.cur.clone(), self.cur_len) {
                        doc.set_kind(&c, "image");
                        c
                    } else {
                        self.start_block(doc, "image")
                    };
                    let hash = doc.attach(bytes, inputs);
                    doc.set_attr(&id, "blob", hash.as_str());
                    let anchor = if !dr.floating {
                        "inline"
                    } else if dr.page_relative {
                        "page"
                    } else {
                        "paragraph"
                    };
                    doc.set_attr(&id, "anchor", anchor);
                    if let Some(w) = dr.width
                        && w > 0
                    {
                        doc.set_attr(&id, "width", w);
                    }
                    if let Some(h) = dr.height
                        && h > 0
                    {
                        doc.set_attr(&id, "height", h);
                    }
                    if let Some(w) = dr.wrap {
                        doc.set_attr(&id, "wrap", w.as_str());
                    }
                    // An image block isn't a text container; close it. What follows the image
                    // in its paragraph continues in a new paragraph block.
                    self.prev = Some(id);
                    self.cur = None;
                    self.continue_para = self.in_para && self.table_id.is_none();
                }
                self.in_run = false;
            }
            _ => {}
        }
        true
    }

    /// Block-structure and run-open events: headings/paragraphs/lists/tables, tracked-change and
    /// hyperlink openers, and the fidelity-atom capture start for anything unmodeled.
    fn handle_start(
        &mut self,
        e: &BytesStart,
        doc: &mut DocMut,
        pos_before: usize,
        maps: &RefMaps,
    ) {
        if self.begin_unmodeled(e, "", pos_before, true, doc) {
            return;
        }
        match e.name().as_ref() {
            b"w:body" => self.in_body = true,
            // A paragraph inside a table cell adds to the cell's text, on a new line after the first.
            b"w:p" if self.table_id.is_some() => {
                self.in_para = true;
                if self.cell_paras > 0 {
                    self.append(doc, "\n");
                }
                self.cell_paras = self.cell_paras.saturating_add(1);
            }
            // A paragraph (outside a table) opens a block; its kind is refined once `w:pPr` closes.
            b"w:p" if self.in_body && self.table_id.is_none() => {
                self.in_para = true;
                self.continue_para = false;
                self.start_block(doc, "paragraph");
                self.p_style = None;
                self.p_align = None;
                self.p_numid = None;
                self.p_ilvl = 0;
                self.p_line_spacing = None;
                self.cur_para_marks.clear();
            }
            b"w:pPr" if self.in_para => {
                self.in_ppr = true;
                self.ppr_kept.clear();
                self.ppr_style = None;
                self.ppr_depth = 0;
            }
            b"w:numPr" if self.in_ppr => self.in_numpr = true,
            // A tracked-change wrapper: create the change record now, remember the start index; the mark applies over `[start, cur_len)` at close.
            b"w:ins" | b"w:del" if self.in_body && self.cur.is_some() => {
                let kind = if e.name().as_ref() == b"w:ins" {
                    tilo_doc::snapshot::ChangeKind::Ins
                } else {
                    tilo_doc::snapshot::ChangeKind::Del
                };
                let author = attr_val(e, b"w:author").unwrap_or_default();
                let date = attr_val(e, b"w:date").unwrap_or_default();
                let cid = doc.insert_change(kind, &author, &date);
                self.change = Some((cid, kind, self.cur.clone(), self.cur_len));
            }
            b"w:hyperlink" if self.in_body && self.cur.is_some() => self.open_hyperlink(e, maps),
            b"w:r" if self.in_body => {
                self.in_run = true;
                self.run_start = self.cur_len;
                self.run_pos = pos_before;
                self.rpr_xml.clear();
                self.run_marks.clear();
            }
            b"w:rPr" if self.in_run => {
                self.in_char_props = true;
                self.rpr_start = pos_before;
            }
            // An embedded image: begin collecting size/positioning/blip until `</w:drawing>`.
            b"w:drawing" if self.in_body => {
                self.drawing = Some(DrawingState::default());
                self.drawing_pos = pos_before;
            }
            // Deleted text in a `<w:del>` run uses `<w:delText>`; treated like `<w:t>` on import.
            b"w:t" | b"w:delText" if self.in_run => self.in_t = true,
            b"w:tbl" if self.in_body => {
                self.table_seq = self.table_seq.saturating_add(1);
                self.table_id = Some(format!("t{}", self.table_seq));
                self.trow = 0;
                self.merges.clear();
                self.table_pr = None;
                self.table_grid = None;
            }
            b"w:tr" if self.table_id.is_some() => {
                self.tcol = 0;
                self.row_pr = None;
            }
            b"w:tc" if self.table_id.is_some() => {
                self.cell_paras = 0;
                self.cell = CellState {
                    id: None,
                    prev: self.prev.clone(),
                    span: 1,
                    merged: false,
                };
                if let Some(tid) = self.table_id.clone() {
                    let id = self.start_block(doc, "cell");
                    self.cell.id = Some(id.clone());
                    doc.set_attr(&id, "table", tid.as_str());
                    if let (Ok(row), Ok(col)) = (i64::try_from(self.trow), i64::try_from(self.tcol))
                    {
                        doc.set_attr(&id, "row", row);
                        doc.set_attr(&id, "col", col);
                    }
                }
            }
            // Unknown block-level element → capture verbatim as a fidelity atom.
            _ if self.in_body && self.cur.is_none() && self.table_id.is_none() => {
                self.capture = Some((pos_before, 1));
            }
            _ => {}
        }
    }

    /// Self-closing property/marker events: paragraph and run properties, tabs/breaks, note
    /// references, comment/bookmark range markers, and the fidelity-atom capture for a single
    /// unmodeled empty element.
    fn handle_empty(
        &mut self,
        e: &BytesStart,
        doc: &mut DocMut,
        reg: &StyleRegistry,
        maps: &RefMaps,
        raw: &str,
        pos_before: usize,
    ) {
        if self.begin_unmodeled(e, raw, pos_before, false, doc) {
            return;
        }
        match e.name().as_ref() {
            b"w:rPr" if self.in_run => self.rpr_xml = raw.to_string(),
            b"w:gridSpan" if self.table_id.is_some() => self.grid_span(e, doc),
            b"w:vMerge" if self.table_id.is_some() => self.vertical_merge(e, doc),
            b"w:p" if self.in_body && self.table_id.is_none() => {
                self.start_block(doc, "paragraph");
                self.cur = None;
            }
            b"w:pStyle" if self.in_ppr => self.p_style = attr_val(e, b"w:val"),
            b"w:jc" if self.in_ppr => self.p_align = attr_val(e, b"w:val").map(|v| map_jc(&v)),
            b"w:spacing" if self.in_ppr => {
                if let Some(ls) = line_spacing_from_spacing(e) {
                    self.p_line_spacing = Some(ls);
                }
            }
            b"w:numId" if self.in_numpr => self.p_numid = attr_val(e, b"w:val"),
            b"w:ilvl" if self.in_numpr => {
                self.p_ilvl = attr_val(e, b"w:val")
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0)
                    .clamp(0, 8);
            }
            b"w:tab" if self.in_run => self.append(doc, "\t"),
            b"w:br" | b"w:cr" if self.in_run => self.append(doc, "\n"),
            b"w:footnoteReference" | b"w:endnoteReference" if self.in_run => {
                // A note reference: resolve its docx id to a qdoc note id, splice a placeholder, and defer the marker until text is present.
                let is_foot = e.name().as_ref() == b"w:footnoteReference";
                let map = if is_foot { maps.foot } else { maps.end };
                if let Some(docx_id) = attr_val(e, b"w:id")
                    && let Some(note_id) = map.get(&docx_id)
                    && let Some(b) = self.cur.clone()
                {
                    let idx = self.cur_len;
                    self.append(doc, &FIELD_PLACEHOLDER.to_string());
                    self.note_refs.push((b, idx, note_id.clone()));
                }
            }
            b"w:commentRangeStart" if self.in_body && self.cur.is_some() => {
                // The start of a commented range: remember the char index; the matching `w:commentRangeEnd` records the end.
                if let Some(docx_id) = attr_val(e, b"w:id")
                    && let Some(cid) = maps.comment.get(&docx_id)
                    && let Some(b) = self.cur.clone()
                {
                    self.open_comments
                        .insert(docx_id, (cid.clone(), b, self.cur_len));
                }
            }
            b"w:commentRangeEnd" if self.in_body => {
                // Closes a commented range: anchor a `comment` mark over `[start, cur_len)`.
                if let Some(docx_id) = attr_val(e, b"w:id")
                    && let Some((cid, b, start)) = self.open_comments.remove(&docx_id)
                {
                    self.comment_ranges.push((b, start, self.cur_len, cid));
                }
            }
            b"w:bookmarkStart" if self.in_body && self.cur.is_some() => {
                // Opens a bookmark's range: remember name + start index. Word's built-in `_GoBack` bookmark has no useful name, so it's skipped.
                if let (Some(id), Some(name), Some(b)) = (
                    attr_val(e, b"w:id"),
                    attr_val(e, b"w:name"),
                    self.cur.clone(),
                ) && name != "_GoBack"
                {
                    self.open_bookmarks.insert(id, (b, self.cur_len, name));
                }
            }
            b"w:bookmarkEnd" if self.in_body => {
                // Closes a bookmark's range: anchor a `bookmark` mark valued with the name.
                if let Some(id) = attr_val(e, b"w:id")
                    && let Some((b, start, name)) = self.open_bookmarks.remove(&id)
                {
                    self.bookmark_ranges.push((b, start, self.cur_len, name));
                }
            }
            // `w:commentReference` carries no range info (defined by start/end), so it's ignored.
            _ if self.in_char_props => {
                if let Some((k, v)) = mark_from_rpr_child(e) {
                    self.run_marks.retain(|(k2, _)| k2 != &k);
                    self.run_marks.push((k, v));
                } else if e.name().as_ref() == b"w:rStyle"
                    && let Some(name) = attr_val(e, b"w:val")
                {
                    for (k, v) in reg.table.text_marks(&name) {
                        self.run_marks.retain(|(k2, _)| k2 != &k);
                        self.run_marks.push((k, v));
                    }
                }
            }
            _ if self.in_body && self.cur.is_none() && self.table_id.is_none() => {
                let id = self.start_block(doc, "fidelity");
                doc.set_attr(&id, "xml", raw);
                doc.set_attr(&id, ATTR_FORMAT, "docx");
                self.cur = None;
            }
            _ => {}
        }
    }

    /// Closing events: property scope ends, tracked-change/hyperlink/run closes (which anchor
    /// their deferred marks), and paragraph/table-cell/table-row/table closes.
    fn handle_end(
        &mut self,
        e: &BytesEnd,
        doc: &mut DocMut,
        reg: &StyleRegistry,
        s: &str,
        pos_after: usize,
    ) {
        match e.name().as_ref() {
            b"w:body" => self.in_body = false,
            b"w:pPr" if self.table_id.is_none() => {
                self.in_ppr = false;
                self.finalize_ppr(doc, reg);
            }
            b"w:pPr" => {
                self.in_ppr = false;
                // A cell's (first) paragraph properties ride on the cell.
                let kept = self.ppr_style.take().unwrap_or_default() + &self.ppr_kept;
                if self.cell_paras == 1
                    && !kept.is_empty()
                    && let Some(id) = self.cur.as_deref()
                {
                    doc.set_attr(id, super::paragraph::ATTR_PPR, kept.as_str());
                }
            }
            b"w:numPr" => self.in_numpr = false,
            b"w:rPr" => {
                if self.in_char_props {
                    self.rpr_xml = s
                        .get(self.rpr_start..pos_after)
                        .unwrap_or_default()
                        .to_string();
                }
                self.in_char_props = false;
            }
            b"w:t" | b"w:delText" => self.in_t = false,
            b"w:ins" | b"w:del" => {
                // Close the tracked-change wrapper: anchor an `ins`/`del` mark over `[start, cur_len)`.
                if let Some((cid, kind, block, start)) = self.change.take()
                    && let Some(b) = block
                {
                    self.change_ranges.push((b, start, self.cur_len, kind, cid));
                }
            }
            b"w:r" => {
                self.in_run = false;
                self.keep_run_props();
                let marks = std::mem::take(&mut self.run_marks);
                if let Some(b) = self.cur.as_ref() {
                    for (name, value) in marks {
                        self.pending_marks.push((
                            b.clone(),
                            self.run_start,
                            self.cur_len,
                            name,
                            value,
                        ));
                    }
                }
            }
            b"w:hyperlink" => {
                // Close the cross-reference: anchor a `link` mark valued `#name` over `[start, cur_len)`.
                if let Some((value, block, start)) = self.hyperlink.take()
                    && let Some(b) = block
                {
                    self.cross_refs.push((b, start, self.cur_len, value));
                }
            }
            b"w:p" if self.table_id.is_none() => {
                self.flush_para_marks();
                self.cur = None;
                self.in_para = false;
                self.continue_para = false;
            }
            b"w:p" => self.in_para = false,
            b"w:tc" if self.table_id.is_some() => {
                self.cur = None;
                // The table's and the row's properties ride on their first cell with a block.
                if let Some(id) = self.cell.id.take() {
                    for (key, value) in [
                        (super::paragraph::ATTR_TBLPR, self.table_pr.take()),
                        (super::paragraph::ATTR_TBLGRID, self.table_grid.take()),
                        (super::paragraph::ATTR_TRPR, self.row_pr.take()),
                    ] {
                        if let Some(value) = value {
                            doc.set_attr(&id, key, value.as_str());
                        }
                    }
                }
                // A cell that is not part of a vertical merge ends any merge in its column.
                if !self.cell.merged {
                    self.merges.remove(&self.tcol);
                }
                self.tcol = self.tcol.saturating_add(self.cell.span);
            }
            b"w:tr" if self.table_id.is_some() => self.trow = self.trow.saturating_add(1),
            b"w:tbl" => {
                self.table_id = None;
                self.cur = None;
            }
            _ => {}
        }
    }

    fn handle_text(&mut self, t: &BytesText, doc: &mut DocMut) -> Result<(), CodecError> {
        if self.in_t && (self.cur.is_some() || self.continue_para) {
            let txt = t
                .xml10_content()
                .map_err(|e| CodecError::Parse(format!("text: {e}")))?;
            self.append_text(&txt, doc);
        }
        Ok(())
    }

    /// An entity or character reference inside `w:t` is the character it stands for.
    fn handle_general_ref(&mut self, r: &BytesRef, doc: &mut DocMut) {
        if self.in_t
            && let Some(c) = crate::resolve_general_ref(r)
        {
            self.append_text(c.encode_utf8(&mut [0; 4]), doc);
        }
    }

    fn append_text(&mut self, txt: &str, doc: &mut DocMut) {
        self.ensure_continuation(doc);
        if let Some(b) = self.cur.as_ref()
            && !txt.is_empty()
        {
            doc.splice_text(b, self.cur_len, 0, txt);
            self.cur_len = self.cur_len.saturating_add(txt.chars().count());
        }
    }

    /// Applies every mark gathered during parsing (runs, paragraph styles, notes, comments,
    /// bookmarks, tracked changes, cross-references), now that all text is present (exact
    /// ranges, no expansion). Called once after the event loop ends.
    fn finish(mut self, doc: &mut DocMut) {
        for (b, start, end, name, value) in self.pending_marks {
            if start >= end {
                continue;
            }
            match value {
                MarkValue::Bool(v) => doc.add_mark(&b, start, end, &name, v),
                MarkValue::Int(n) => doc.add_mark(&b, start, end, &name, n),
                MarkValue::Str(s) => doc.add_mark(&b, start, end, &name, s.as_str()),
            }
        }
        for (b, idx, nid) in self.note_refs {
            let end = idx.saturating_add(1);
            doc.add_mark(&b, idx, end, MARK_NOTE_REF, nid.as_str());
        }
        for (b, start, end, xml) in self.atoms {
            doc.add_mark(&b, start, end, MARK_DOCX_XML, xml.as_str());
        }
        for (b, start, end, kept) in self.rpr_marks {
            doc.add_mark(&b, start, end, super::paragraph::MARK_RPR, kept.as_str());
        }
        // A comment missing its `w:commentRangeEnd` anchors at start, extending to block end.
        for (_docx_id, (cid, b, start)) in std::mem::take(&mut self.open_comments) {
            let end = doc_block_len(doc, &b).max(start);
            self.comment_ranges.push((b, start, end, cid));
        }
        // A zero-width comment range can't carry a mark, so it's widened to one character when the block has text after the anchor.
        for (b, start, mut end, cid) in self.comment_ranges {
            if end <= start {
                let blen = doc_block_len(doc, &b);
                end = start.saturating_add(1).min(blen);
            }
            if end > start {
                doc.add_mark(&b, start, end, MARK_COMMENT, cid.as_str());
            }
        }
        // Tracked-change anchor marks: cover each changed range with the matching `ins`/`del` mark.
        for (b, start, end, kind, cid) in self.change_ranges {
            if end > start {
                doc.add_change_mark(kind, &b, start, end, &cid);
            }
        }
        // A bookmark missing its `w:bookmarkEnd` anchors at start, extending to block end.
        for (_id, (b, start, name)) in std::mem::take(&mut self.open_bookmarks) {
            let end = doc_block_len(doc, &b).max(start);
            self.bookmark_ranges.push((b, start, end, name));
        }
        // Bookmark anchor marks: a zero-width (point) range is widened to one character (see `add_bookmark`).
        for (b, start, end, name) in self.bookmark_ranges {
            doc.add_bookmark(&b, start, end, &name);
        }
        // Cross-reference `link` marks cover each linked range, valued `#target`.
        for (b, start, end, value) in self.cross_refs {
            if end > start {
                doc.add_mark(&b, start, end, tilo_doc::schema::MARK_LINK, value.as_str());
            }
        }
    }
}

pub(crate) fn parse_body(
    document_xml: &[u8],
    reg: &StyleRegistry,
    maps: &RefMaps,
    image_map: &BTreeMap<String, Vec<u8>>,
    doc: &mut DocMut,
    inputs: &mut ContentInputs,
) -> Result<(), CodecError> {
    let s = std::str::from_utf8(document_xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);
    let mut st = BodyState::default();

    loop {
        let pos_before = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let ev = reader
            .read_event()
            .map_err(|e| CodecError::Parse(format!("xml: {e}")))?;
        let pos_after = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);

        if st.handle_inline(s, pos_before, pos_after, &ev, doc) {
            continue;
        }
        if st.handle_capture(s, pos_after, &ev, doc) {
            continue;
        }
        if st.handle_drawing(&ev, doc, image_map, inputs, s, pos_after) {
            continue;
        }

        st.observe_ppr(&ev, s, pos_before, pos_after);
        st.observe_table(&ev, s, pos_before, pos_after, doc);
        match ev {
            Event::Start(e) => st.handle_start(&e, doc, pos_before, maps),
            Event::Empty(e) => {
                let raw = s.get(pos_before..pos_after).unwrap_or_default();
                st.handle_empty(&e, doc, reg, maps, raw, pos_before);
            }
            Event::End(e) => st.handle_end(&e, doc, reg, s, pos_after),
            Event::Text(t) => st.handle_text(&t, doc)?,
            Event::GeneralRef(r) => st.handle_general_ref(&r, doc),
            Event::Eof => break,
            _ => {}
        }
    }

    st.finish(doc);
    Ok(())
}

/// The character length of a body block's text (0 if absent).
fn doc_block_len(doc: &DocMut, block_id: &str) -> usize {
    doc.block_text_len(block_id)
}

#[cfg(test)]
mod tests {
    use super::super::DocxCodec;
    use super::super::container::{minimal_docx, minimal_docx_numbered, minimal_docx_styled};
    use crate::FormatProvider;
    use tilo_doc::{AttrVal, Document};

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn imports_block_structure_lists_tables_and_fidelity_atoms() {
        // Phase `imports_headings_and_paragraphs`.
        {
            let body = concat!(
                r#"<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>"#,
                r#"<w:p><w:r><w:t>Hello</w:t></w:r></w:p>"#,
            );
            let docx = minimal_docx(body);
            let mut d = Document::new();
            DocxCodec
                .import(
                    &docx,
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
        }
        // Phase `imports_heading_via_named_style_with_style_alignment`.
        {
            // A custom style derived from Heading1 that also centres text: heading detection + alignment come from the resolved style.
            let styles = concat!(
                r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/><w:jc w:val="center"/></w:pPr></w:style>"#,
                r#"<w:style w:type="paragraph" w:styleId="MyTitle"><w:basedOn w:val="Heading1"/></w:style>"#,
            );
            let body = r#"<w:p><w:pPr><w:pStyle w:val="MyTitle"/></w:pPr><w:r><w:t>My Title</w:t></w:r></w:p>"#;
            let docx = minimal_docx_styled(styles, body);
            let mut d = Document::new();
            DocxCodec
                .import(
                    &docx,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let b = &d.materialize().blocks[0];
            assert_eq!(b.kind, "heading", "named paragraph style → heading");
            assert_eq!(b.text, "My Title");
            assert_eq!(
                b.attrs.get("level"),
                Some(&AttrVal::Int(1)),
                "level inherited via basedOn"
            );
            assert_eq!(
                b.attrs.get("align"),
                Some(&AttrVal::Str("center".into())),
                "alignment from the style"
            );
        }
        // Phase `imports_numbered_and_bulleted_lists`.
        {
            let numbering = concat!(
                r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>"#,
                r#"<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>"#,
                r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
                r#"<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>"#,
            );
            let body = concat!(
                r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>first</w:t></w:r></w:p>"#,
                r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>bee</w:t></w:r></w:p>"#,
            );
            let docx = minimal_docx_numbered("", numbering, body);
            let mut d = Document::new();
            DocxCodec
                .import(
                    &docx,
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
            assert_eq!(items.len(), 2, "{snap:?}");
            assert_eq!(items[0].text, "first");
            assert_eq!(
                items[0].attrs.get("ordered"),
                Some(&AttrVal::Bool(true)),
                "decimal numId → ordered"
            );
            assert_eq!(
                items[1].attrs.get("ordered"),
                Some(&AttrVal::Bool(false)),
                "bullet numId → unordered"
            );
        }
        // Phase `imports_table_as_editable_cells`.
        {
            let body = concat!(
                r#"<w:tbl>"#,
                r#"<w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr>"#,
                r#"<w:tr><w:tc><w:p><w:r><w:t>c</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>d</w:t></w:r></w:p></w:tc></w:tr>"#,
                r#"</w:tbl>"#,
            );
            let docx = minimal_docx(body);
            let mut d = Document::new();
            DocxCodec
                .import(
                    &docx,
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
            assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
            assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));
        }
        // Phase `captures_unmodeled_body_element_as_fidelity_atom`.
        {
            let body = concat!(
                r#"<w:p><w:r><w:t>before</w:t></w:r></w:p>"#,
                r#"<w:sdt><w:sdtContent><w:r><w:t>boxed</w:t></w:r></w:sdtContent></w:sdt>"#,
                r#"<w:p><w:r><w:t>after</w:t></w:r></w:p>"#,
            );
            let docx = minimal_docx(body);
            let mut d = Document::new();
            DocxCodec
                .import(
                    &docx,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();
            assert_eq!(snap.blocks.len(), 3, "before / fidelity / after: {snap:?}");
            assert_eq!(snap.blocks[0].text, "before");
            assert_eq!(snap.blocks[1].kind, "fidelity");
            assert_eq!(snap.blocks[2].text, "after");
            let raw = match snap.blocks[1].attrs.get("xml") {
                Some(AttrVal::Str(x)) => x.clone(),
                other => panic!("fidelity atom missing xml attr: {other:?}"),
            };
            assert!(
                raw.starts_with("<w:sdt") && raw.contains("boxed") && raw.ends_with("</w:sdt>"),
                "captured raw: {raw}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn imports_run_and_paragraph_formatting_and_named_styles() {
        // Phase `imports_run_props_as_marks`.
        {
            use tilo_doc::document::MarkValue;
            let body = concat!(
                r#"<w:p>"#,
                r#"<w:r><w:t xml:space="preserve">a </w:t></w:r>"#,
                r#"<w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r>"#,
                r#"<w:r><w:t xml:space="preserve"> and </w:t></w:r>"#,
                r#"<w:r><w:rPr><w:i/><w:u w:val="single"/><w:strike/></w:rPr><w:t>more</w:t></w:r>"#,
                r#"</w:p>"#,
            );
            let docx = minimal_docx(body);
            let mut d = Document::new();
            DocxCodec
                .import(
                    &docx,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let p = &d.materialize().blocks[0];
            assert_eq!(p.text, "a bold and more");
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "strong" && m.start == 2 && m.end == 6),
                "{:?}",
                p.marks
            );
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "em" && m.start == 11 && m.end == 15),
                "{:?}",
                p.marks
            );
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "underline" && m.start == 11 && m.end == 15),
                "{:?}",
                p.marks
            );
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "strike" && m.start == 11 && m.end == 15),
                "{:?}",
                p.marks
            );
            let _ = MarkValue::Bool(true);
        }
        // Phase `imports_align_font_size_and_family`.
        {
            use tilo_doc::document::MarkValue;
            let body = concat!(
                r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr>"#,
                r#"<w:r><w:rPr><w:sz w:val="40"/><w:rFonts w:ascii="Times New Roman"/></w:rPr><w:t>big</w:t></w:r>"#,
                r#"</w:p>"#,
            );
            let docx = minimal_docx(body);
            let mut d = Document::new();
            DocxCodec
                .import(
                    &docx,
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
        // Phase `imports_named_character_style_and_whitespace`.
        {
            let styles =
                r#"<w:style w:type="character" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style>"#;
            let body = concat!(
                r#"<w:p><w:r><w:t>x</w:t></w:r>"#,
                r#"<w:r><w:rPr><w:rStyle w:val="Strong"/></w:rPr><w:t>y</w:t></w:r></w:p>"#,
                r#"<w:p><w:r><w:t>one</w:t><w:br/><w:t>two</w:t><w:tab/><w:t>three</w:t></w:r></w:p>"#,
            );
            let docx = minimal_docx_styled(styles, body);
            let mut d = Document::new();
            DocxCodec
                .import(
                    &docx,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();
            let p0 = &snap.blocks[0];
            assert_eq!(p0.text, "xy");
            assert!(
                p0.marks
                    .iter()
                    .any(|m| m.name == "strong" && m.start == 1 && m.end == 2),
                "named char style → mark: {:?}",
                p0.marks
            );
            let p1 = &snap.blocks[1];
            assert!(p1.text.contains('\n'), "w:br → newline: {:?}", p1.text);
            assert!(p1.text.contains('\t'), "w:tab → tab: {:?}", p1.text);
            assert_eq!(p1.text, "one\ntwo\tthree");
        }
        // Phase `imports_named_paragraph_style_into_container`.
        {
            // A custom paragraph style carrying formatting lifts into the styles container, pointed at via `w:styleId`.
            let styles = concat!(
                r##"<w:style w:type="paragraph" w:styleId="Fancy"><w:name w:val="Fancy"/>"##,
                r##"<w:pPr><w:jc w:val="center"/><w:spacing w:after="180"/><w:ind w:left="360"/></w:pPr>"##,
                r##"<w:rPr><w:rFonts w:ascii="Times New Roman"/><w:b/><w:i/><w:color w:val="FF0000"/><w:sz w:val="40"/></w:rPr></w:style>"##,
            );
            let body =
                r#"<w:p><w:pPr><w:pStyle w:val="Fancy"/></w:pPr><w:r><w:t>hi</w:t></w:r></w:p>"#;
            let docx = minimal_docx_styled(styles, body);
            let mut d = Document::new();
            DocxCodec
                .import(
                    &docx,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let snap = d.materialize();
            let b = &snap.blocks[0];
            assert_eq!(b.kind, "paragraph");
            assert_eq!(b.text, "hi");
            assert_eq!(
                b.attrs.get("style"),
                Some(&AttrVal::Str("Fancy".into())),
                "block points at the styleId: {:?}",
                b.attrs
            );
            // The formatting lives in the style, not directly on the block.
            assert_eq!(
                b.attrs.get("align"),
                None,
                "alignment comes from the style, not a direct attr: {:?}",
                b.attrs
            );
            assert!(
                b.marks.is_empty(),
                "style formatting not lifted as inline marks: {:?}",
                b.marks
            );

            let st = snap
                .styles
                .get("Fancy")
                .expect("style lifted into container");
            assert_eq!(st.get("name"), Some(&AttrVal::Str("Fancy".into())));
            assert_eq!(st.get("bold"), Some(&AttrVal::Bool(true)));
            assert_eq!(st.get("italic"), Some(&AttrVal::Bool(true)));
            assert_eq!(
                st.get("color"),
                Some(&AttrVal::Str("#ff0000".into())),
                "w:color FF0000 → #ff0000"
            );
            assert_eq!(st.get("align"), Some(&AttrVal::Str("center".into())));
            assert_eq!(
                st.get("size"),
                Some(&AttrVal::Float(20.0)),
                "w:sz 40 half-points → 20pt"
            );
            assert_eq!(
                st.get("font"),
                Some(&AttrVal::Str("serif".into())),
                "Times New Roman → serif"
            );
            assert_eq!(
                st.get("space_after"),
                Some(&AttrVal::Float(12.0)),
                "180 twips → 12px"
            );
            assert_eq!(
                st.get("indent"),
                Some(&AttrVal::Float(24.0)),
                "360 twips → 24px"
            );
        }
    }

    #[test]
    fn imports_and_roundtrips_document_and_run_language() {
        // Phase `imports_run_language_as_mark`.
        {
            use tilo_doc::document::MarkValue;
            // A run carrying `<w:lang w:val="fr"/>` → a `lang` run mark with the tag.
            let body = concat!(
                r#"<w:p>"#,
                r#"<w:r><w:t xml:space="preserve">a </w:t></w:r>"#,
                r#"<w:r><w:rPr><w:lang w:val="fr"/></w:rPr><w:t>bonjour</w:t></w:r>"#,
                r#"</w:p>"#,
            );
            let docx = minimal_docx(body);
            let mut d = Document::new();
            DocxCodec
                .import(
                    &docx,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            let p = &d.materialize().blocks[0];
            assert_eq!(p.text, "a bonjour");
            assert!(
                p.marks.iter().any(|m| m.name == "lang"
                    && m.start == 2
                    && m.end == 9
                    && m.value == MarkValue::Str("fr".into())),
                "lang mark over 'bonjour': {:?}",
                p.marks
            );
        }
        // Phase `imports_document_default_language`.
        {
            // `docDefaults > rPrDefault > rPr > w:lang` sets the document default; the style's own `w:lang` proves only the run default is read.
            let styles = concat!(
                r#"<w:docDefaults><w:rPrDefault><w:rPr><w:lang w:val="de-DE"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
                r#"<w:style w:type="paragraph" w:styleId="Body"><w:rPr><w:lang w:val="en-US"/></w:rPr></w:style>"#,
            );
            let body = r"<w:p><w:r><w:t>hi</w:t></w:r></w:p>";
            let docx = minimal_docx_styled(styles, body);
            let mut d = Document::new();
            DocxCodec
                .import(
                    &docx,
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            assert_eq!(
                d.materialize().language.as_deref(),
                Some("de-DE"),
                "document default language from docDefaults"
            );
        }
        // Phase `document_and_run_language_roundtrip`.
        {
            use tilo_doc::document::MarkValue;
            // A `de-DE` default language plus a `fr`-marked run must both survive export→import.
            let mut d = Document::new();
            d.set_language("de-DE");
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "bonjour monde");
            d.add_mark(&id, 0, 7, "lang", "fr");

            let exported = DocxCodec
                .export(d.as_ref(), &tilo_content::NoContent)
                .unwrap();
            let mut d2 = Document::new();
            DocxCodec
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
        let field = concat!(
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
            r#"<w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>7</w:t></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
        );
        let sdt = r#"<w:sdt><w:sdtPr><w:tag w:val="t"/></w:sdtPr><w:sdtContent><w:r><w:t>ctl</w:t></w:r></w:sdtContent></w:sdt>"#;
        let page_break = r#"<w:r><w:rPr><w:b/></w:rPr><w:br w:type="page"/></w:r>"#;
        let body = format!(
            r#"<w:p><w:r><w:t xml:space="preserve">page </w:t></w:r>{field}<w:r><w:t xml:space="preserve"> and </w:t></w:r>{sdt}{page_break}<w:r><w:t>end</w:t></w:r></w:p>"#
        );
        let mut d = Document::new();
        DocxCodec
            .import(
                &minimal_docx(&body),
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        let p = &d.materialize().blocks[0];
        let ph = tilo_doc::schema::FIELD_PLACEHOLDER;
        // The field and the content control show their text; the page break shows none.
        assert_eq!(p.text, format!("page 7 and ctl{ph}end"));
        let atoms = p
            .marks
            .iter()
            .filter(|m| m.name == crate::preserve::MARK_DOCX_XML)
            .count();
        assert_eq!(atoms, 3, "{:?}", p.marks);

        // An edit through the model, then export: every piece re-emits verbatim, in place.
        let id = p.id.clone();
        d.splice_text(&id, 0, 0, "The ");
        let out = DocxCodec
            .export(d.as_ref(), &tilo_content::NoContent)
            .unwrap();
        let c = ZipPackage::read(&out).unwrap();
        let xml = String::from_utf8(c.get("word/document.xml").unwrap().to_vec()).unwrap();
        let at = |needle: &str| {
            xml.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {xml}"))
        };
        assert!(at("The page") < at(field) && at(field) < at(sdt) && at(sdt) < at(page_break));
        assert!(at(page_break) < at(">end</w:t>"));
        let mut again = Document::new();
        DocxCodec
            .import(
                &out,
                &mut again.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        assert_eq!(
            again.materialize().blocks[0].text,
            format!("The page 7 and ctl{ph}end")
        );

        // Editing a kept element's text writes the edited text instead of the element.
        let at_ctl = "The page 7 and ".chars().count();
        d.splice_text(&id, at_ctl, 3, "CTL");
        let out = DocxCodec
            .export(d.as_ref(), &tilo_content::NoContent)
            .unwrap();
        let c = ZipPackage::read(&out).unwrap();
        let xml = String::from_utf8(c.get("word/document.xml").unwrap().to_vec()).unwrap();
        assert!(
            !xml.contains("<w:sdt>") && xml.contains(" and CTL</w:t>"),
            "{xml}"
        );
        assert!(xml.contains(field), "the unedited field stays: {xml}");
    }

    #[test]
    fn text_after_an_inline_picture_continues_after_it() {
        let body = concat!(
            r#"<w:p><w:r><w:t>before</w:t></w:r>"#,
            r#"<w:r><w:drawing><wp:inline><wp:extent cx="9525" cy="9525"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rIdMissing"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#,
            r#"<w:r><w:t>after</w:t></w:r></w:p>"#,
        );
        let mut d = Document::new();
        DocxCodec
            .import(
                &minimal_docx(body),
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        let snap = d.materialize();
        // The picture's part is missing, so the drawing stays verbatim, inline, between the texts.
        assert_eq!(snap.blocks.len(), 1, "{snap:?}");
        assert_eq!(
            snap.blocks[0].text,
            format!("before{}after", tilo_doc::schema::FIELD_PLACEHOLDER)
        );
    }
}
