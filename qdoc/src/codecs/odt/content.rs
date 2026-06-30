//! Parse the `content.xml` `office:text` body into canonical blocks + marks.
//! Styles (named in `styles.xml`, automatic in `content.xml`) are pre-parsed
//! into a [`StyleRegistry`]; this walk resolves paragraph/heading/list/text
//! styles against it. Headings are recognised from `text:h` *and* from
//! paragraphs using a heading paragraph-style; inline spans, alignment, font,
//! list numbering, tabs/breaks/spaces, and tables all map onto the model.
//! Unmodeled body elements are captured verbatim as fidelity atoms.

use super::container::OdtContainer;
use super::styles::{attr_val, StyleRegistry};
use crate::document::MarkValue;
use crate::format::CodecError;
use crate::model::DocMut;
use crate::schema::{FIELD_PLACEHOLDER, MARK_COMMENT, MARK_NOTE_REF};
use crate::snapshot::ChangeKind;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::BTreeMap;

/// Parse an ODF length (`svg:width`/`svg:height`, e.g. `"3cm"`, `"96px"`, `"1in"`)
/// to an integer pixel count. `px` (and bare numbers) round-trip exactly; other
/// units convert at 96dpi. Returns `None` for unparseable values.
pub(super) fn parse_len_px(v: &str) -> Option<i64> {
    let v = v.trim();
    let split = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let n: f64 = num.trim().parse().ok()?;
    let px = match unit.trim() {
        "px" | "" => n,
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        "in" => n * 96.0,
        "pt" => n * 96.0 / 72.0,
        "pc" => n * 16.0,
        _ => n,
    };
    Some(px.round() as i64)
}

/// In-progress `draw:frame` capture: an image (if its `Pictures/*` part exists)
/// or a verbatim fidelity atom (fallback) when the picture is missing.
struct FrameState {
    raw_start: usize,
    depth: i32,
    width: Option<String>,
    height: Option<String>,
    /// Model anchor (`inline`|`paragraph`|`page`) from `text:anchor-type`.
    anchor: Option<String>,
    /// Model wrap (`none`|`left`|`right`|`both`) resolved from the frame's
    /// graphic style's `style:wrap`.
    wrap: Option<String>,
    href: Option<String>,
    title: String,
    desc: String,
    collecting: Option<bool>, // Some(true)=svg:title, Some(false)=svg:desc
    /// When the frame is nested *inside* a `text:p`, the id of that paragraph
    /// block (so an empty image-only paragraph can be dropped after the image is
    /// emitted as its own block). `None` for a top-level frame.
    enclosing_para: Option<String>,
    /// Whether the enclosing paragraph already held text when the frame opened.
    para_had_text: bool,
}

/// Whether a raw text node is purely formatting whitespace inserted by a
/// pretty-printer (LibreOffice/pandoc indent the structural list/table/heading
/// elements). Such nodes always contain a newline; genuine inline whitespace in
/// ODF flow content is a plain space (real breaks use `text:line-break`, runs of
/// spaces use `text:s`). Dropping these keeps list items / headings tight instead
/// of importing the indentation as empty leading/trailing lines.
fn is_layout_whitespace(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c.is_whitespace()) && t.contains(['\n', '\r'])
}

/// Open a fresh top-level block anchored after the previous one.
fn start_block(doc: &mut DocMut, prev: &mut Option<String>, cur: &mut Option<String>, cur_len: &mut usize, kind: &str) -> String {
    let id = doc.insert_block(prev.as_deref(), kind);
    *prev = Some(id.clone());
    *cur = Some(id.clone());
    *cur_len = 0;
    id
}

fn outline_level(e: &BytesStart) -> Option<i64> {
    attr_val(e, b"text:outline-level").and_then(|v| v.parse::<i64>().ok())
}

/// Append literal text (e.g. tab / line-break / spaces) to the current block.
fn append(doc: &mut DocMut, cur: &Option<String>, cur_len: &mut usize, txt: &str) {
    if let Some(b) = cur {
        if !txt.is_empty() {
            doc.splice_text(b, *cur_len, 0, txt);
            *cur_len += txt.chars().count();
        }
    }
}

/// Open a paragraph or heading depending on the referenced paragraph style, set
/// level/alignment, and return the paragraph-level inline marks to apply over the
/// whole block (empty for headings — their boldness comes from the heading kind).
fn open_para(doc: &mut DocMut, prev: &mut Option<String>, cur: &mut Option<String>, cur_len: &mut usize, reg: &StyleRegistry, style: Option<&str>) -> Vec<(String, MarkValue)> {
    let level = style.and_then(|s| reg.heading_level(s));
    let id = start_block(doc, prev, cur, cur_len, if level.is_some() { "heading" } else { "paragraph" });
    if let Some(l) = level {
        doc.set_attr(&id, "level", l);
    }
    // Line spacing rides as a direct `line_spacing` Float attr (it isn't part of
    // the CRDT named-style props, so it's always lifted onto the block — even when
    // the paragraph references a named style below).
    if let Some(lh) = style.and_then(|s| reg.para_line_height(s)) {
        doc.set_attr(&id, "line_spacing", lh);
    }
    // A *named* (common) non-heading paragraph style → a real CRDT-native style:
    // lift its definition into the styles container and point the block at it via
    // an explicit `style` attr (keyed by the decoded style name). The style now
    // supplies the block's alignment + formatting, so they are not *also* lifted
    // onto the block as direct attrs / inline marks (keeps the round-trip
    // idempotent). Built-in heading/list mapping is untouched.
    if level.is_none() {
        if let Some(s) = style {
            if reg.is_named_para(s) {
                if let Some(props) = reg.style_props(s) {
                    let key = super::styles::decode_style_name(s);
                    doc.set_style(&key, &props);
                    doc.set_attr(&id, "style", key.as_str());
                    return Vec::new();
                }
            }
        }
    }
    // Automatic style or heading: lift alignment as a direct attr.
    if let Some(a) = style.and_then(|s| reg.para_align(s)) {
        doc.set_attr(&id, "align", a.as_str());
    }
    if level.is_some() {
        Vec::new()
    } else {
        style.map(|s| reg.para_text_marks(s)).unwrap_or_default()
    }
}

pub(crate) fn parse_body(content_xml: &[u8], reg: &StyleRegistry, container: &OdtContainer, doc: &mut DocMut) -> Result<(), CodecError> {
    let s = std::str::from_utf8(content_xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);

    let mut in_text = false;
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut in_item = false;

    // Span marks + paragraph-style marks, applied after all text is in (so text
    // appended after a span doesn't expand its mark).
    let mut span_stack: Vec<(Vec<(String, MarkValue)>, usize)> = Vec::new();
    // Open `text:a` hyperlinks: (href, style-marks from the link's own style, start).
    let mut link_stack: Vec<(Option<String>, Vec<(String, MarkValue)>, usize)> = Vec::new();
    let mut pending_marks: Vec<(String, usize, usize, String, MarkValue)> = Vec::new();
    // Paragraph-style inline marks for the current block, applied over [0, len].
    let mut cur_para_marks: Vec<(String, MarkValue)> = Vec::new();
    // Nested list ordered-ness, from each list's list-style.
    let mut list_stack: Vec<bool> = Vec::new();
    // Per-open-list captured numbering for fidelity preservation: the level-0
    // `(num_format, num_start, restart)` derived from the list-style + the
    // `text:list` element's own `text:start-value`/`text:continue-numbering`.
    let mut list_num_stack: Vec<(Option<&'static str>, Option<i64>, bool)> = Vec::new();

    // Table state: cells become consecutive `cell` blocks tagged table/row/col.
    let mut table_id: Option<String> = None;
    let mut trow: usize = 0;
    let mut tcol: usize = 0;
    let mut table_seq: usize = 0;

    let mut capture: Option<(usize, i32)> = None; // (start byte, open depth) of a fidelity atom
    let mut frame: Option<FrameState> = None; // an in-progress draw:frame (image or fidelity)
    // An in-progress inline `text:note` (footnote/endnote): its raw-XML start byte,
    // open depth, and the (block, index) at which to anchor its reference marker.
    // Deferred so the marker is added after all text is in place (exact position).
    let mut note: Option<(usize, i32, Option<String>, usize)> = None;
    // (block, char index, note id) reference markers, applied after all text is in.
    let mut note_refs: Vec<(String, usize, String)> = Vec::new();
    // An in-progress inline `office:annotation` (comment): its raw-XML start byte,
    // open depth, and the (block, start char index) at which the commented range
    // begins. Deferred so the `comment` mark is added after all text is in place.
    let mut annotation: Option<(usize, i32, Option<String>, usize)> = None;
    // Pending comment ranges keyed by the ODF `office:name`: `name → (comment id,
    // block, start char index)`, awaiting their matching `office:annotation-end`.
    let mut open_comments: BTreeMap<String, (String, String, usize)> = BTreeMap::new();
    // (block, start, end, comment id) anchor marks, applied after all text is in.
    let mut comment_ranges: Vec<(String, usize, usize, String)> = Vec::new();

    // Open bookmark ranges keyed by the ODF `text:name`: `name → (block, start char
    // index)`, awaiting their matching `text:bookmark-end`.
    let mut open_bookmarks: BTreeMap<String, (String, usize)> = BTreeMap::new();
    // (block, start, end, bookmark name) anchor marks, applied after all text is in.
    let mut bookmark_ranges: Vec<(String, usize, usize, String)> = Vec::new();

    // An in-progress `text:tracked-changes` declaration block: its raw-XML start byte +
    // open depth. Captured whole, then parsed into change records on close.
    let mut tracked: Option<(usize, i32)> = None;
    // ODF `xml:id` → (qdoc change id, kind), from the parsed tracked-changes block. The
    // body's `text:change-start`/`-end`/`text:change` markers reference changes by this
    // id; the `ins`/`del` mark is applied over the marked range.
    let mut change_decls: BTreeMap<String, (String, ChangeKind)> = BTreeMap::new();
    // Open tracked-change ranges keyed by ODF `xml:id`: `xml:id → (change id, kind,
    // block, start char index)`, awaiting their matching `text:change-end`.
    let mut open_changes: BTreeMap<String, (String, ChangeKind, String, usize)> = BTreeMap::new();
    // (block, start, end, kind, change id) anchor marks, applied after all text is in.
    let mut change_ranges: Vec<(String, usize, usize, ChangeKind, String)> = Vec::new();

    // Flush the current block's paragraph-style marks over its full text range.
    macro_rules! flush_para_marks {
        () => {
            if let Some(b) = cur.as_ref() {
                for (name, value) in cur_para_marks.drain(..) {
                    pending_marks.push((b.clone(), 0, cur_len, name, value));
                }
            }
            cur_para_marks.clear();
        };
    }

    loop {
        let pos_before = reader.buffer_position() as usize;
        let ev = reader.read_event().map_err(|e| CodecError::Parse(format!("xml: {e}")))?;

        if let Some((start, depth)) = capture.as_mut() {
            match &ev {
                Event::Start(_) => *depth += 1,
                Event::End(_) => {
                    *depth -= 1;
                    if *depth == 0 {
                        let raw = s[*start..reader.buffer_position() as usize].to_string();
                        let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "fidelity");
                        doc.set_attr(&id, "xml", raw.as_str());
                        cur = None;
                        capture = None;
                    }
                }
                _ => {}
            }
            continue;
        }

        // Inside a draw:frame: collect the image href (draw:image) + alt
        // (svg:title/svg:desc), then on the frame's close emit either an `image`
        // block (if the Pictures part exists) or a verbatim fidelity atom.
        if let Some(fr) = frame.as_mut() {
            match &ev {
                Event::Start(e) => {
                    fr.depth += 1;
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
                        let txt = t.unescape().map_err(|e| CodecError::Parse(format!("text: {e}")))?;
                        if is_title {
                            fr.title.push_str(&txt);
                        } else {
                            fr.desc.push_str(&txt);
                        }
                    }
                }
                Event::End(e) => {
                    fr.depth -= 1;
                    match e.name().as_ref() {
                        b"svg:title" | b"svg:desc" => fr.collecting = None,
                        _ => {}
                    }
                    if fr.depth == 0 {
                        let end = reader.buffer_position() as usize;
                        let bytes = fr.href.as_deref().and_then(|h| container.get(h).map(|b| b.to_vec()));
                        if let Some(bytes) = bytes {
                            let hash = doc.put_blob(&bytes);
                            let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "image");
                            doc.set_attr(&id, "blob", hash.as_str());
                            let alt = if !fr.title.is_empty() { &fr.title } else { &fr.desc };
                            if !alt.is_empty() {
                                doc.set_attr(&id, "alt", alt.as_str());
                            }
                            if let Some(w) = fr.width.as_deref().and_then(parse_len_px) {
                                doc.set_attr(&id, "width", w);
                            }
                            if let Some(h) = fr.height.as_deref().and_then(parse_len_px) {
                                doc.set_attr(&id, "height", h);
                            }
                            // Positioning: anchor (inline/paragraph/page) + wrap.
                            if let Some(a) = fr.anchor.as_deref() {
                                doc.set_attr(&id, "anchor", a);
                            }
                            if let Some(w) = fr.wrap.as_deref() {
                                doc.set_attr(&id, "wrap", w);
                            }
                            // An image-only paragraph (the common LibreOffice/pandoc
                            // "figure" layout) leaves an empty wrapper paragraph — drop
                            // it so the image isn't preceded by a blank line.
                            if let Some(pid) = fr.enclosing_para.as_deref() {
                                if !fr.para_had_text {
                                    doc.remove_block(pid);
                                }
                            }
                        } else {
                            // Picture missing from the container → preserve verbatim.
                            let raw = s[fr.raw_start..end].to_string();
                            let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "fidelity");
                            doc.set_attr(&id, "xml", raw.as_str());
                        }
                        cur = None;
                        frame = None;
                    }
                }
                _ => {}
            }
            continue;
        }

        // Inside an inline `text:note`: collect its raw XML until it closes, then
        // parse it into a note + record a reference marker at the captured position.
        if let Some((start, depth, block, idx)) = note.as_mut() {
            match &ev {
                Event::Start(_) => *depth += 1,
                Event::End(_) => {
                    *depth -= 1;
                    if *depth == 0 {
                        let raw = s[*start..reader.buffer_position() as usize].to_string();
                        if let (Some(b), Some(nid)) = (block.clone(), super::notes::import_note(&raw, doc)) {
                            note_refs.push((b, *idx, nid));
                        }
                        note = None;
                    }
                }
                _ => {}
            }
            continue;
        }

        // Inside an inline `office:annotation` (comment): collect its raw XML until it
        // closes, parse it into a comment, and remember the start of the commented
        // range so the matching `office:annotation-end` (or, for a point comment, the
        // same position) anchors a `comment` mark.
        if let Some((start, depth, block, idx)) = annotation.as_mut() {
            match &ev {
                Event::Start(_) => *depth += 1,
                Event::End(_) => {
                    *depth -= 1;
                    if *depth == 0 {
                        let raw = s[*start..reader.buffer_position() as usize].to_string();
                        if let (Some(b), Some((name, cid))) = (block.clone(), super::comments::import_annotation(&raw, doc)) {
                            match name {
                                // A ranged comment: stash it until its annotation-end.
                                Some(n) => {
                                    open_comments.insert(n, (cid, b, *idx));
                                }
                                // A point comment (no office:name): anchor a 1-char
                                // range at the insertion point if there's a character
                                // there, else a zero-width range that import drops.
                                None => comment_ranges.push((b, *idx, *idx, cid)),
                            }
                        }
                        annotation = None;
                    }
                }
                _ => {}
            }
            continue;
        }

        // Inside the `text:tracked-changes` declaration block: capture its raw XML until
        // it closes, then parse the changed-region declarations into change records and
        // record an `xml:id → (change id, kind)` map for the body markers.
        if let Some((start, depth)) = tracked.as_mut() {
            match &ev {
                Event::Start(_) => *depth += 1,
                Event::End(_) => {
                    *depth -= 1;
                    if *depth == 0 {
                        let raw = s[*start..reader.buffer_position() as usize].to_string();
                        for decl in super::changes::import_tracked_changes(&raw) {
                            let cid = doc.insert_change(decl.kind, &decl.author, &decl.date);
                            change_decls.insert(decl.xml_id, (cid, decl.kind));
                        }
                        tracked = None;
                    }
                }
                _ => {}
            }
            continue;
        }

        match ev {
            Event::Start(e) => match e.name().as_ref() {
                b"text:note" if in_text && cur.is_some() => {
                    // A footnote/endnote reference: splice the placeholder char now
                    // (so later text in this paragraph lands after it), capture the
                    // whole element verbatim, and defer the `note-ref` mark until all
                    // text is in place. The note's body is parsed on close.
                    let idx = cur_len;
                    append(doc, &cur, &mut cur_len, &FIELD_PLACEHOLDER.to_string());
                    note = Some((pos_before, 1, cur.clone(), idx));
                }
                b"office:annotation" if in_text && cur.is_some() => {
                    // A comment anchored to a range: the annotation sits at the *start*
                    // of the commented text (no placeholder — it spans real chars).
                    // Capture the whole element verbatim and remember the current char
                    // index; the matching `office:annotation-end` records the range end.
                    let idx = cur_len;
                    annotation = Some((pos_before, 1, cur.clone(), idx));
                }
                b"text:tracked-changes" if in_text => {
                    // The up-front tracked-change declaration block: capture it whole and
                    // parse it into change records (see the capture handler above).
                    tracked = Some((pos_before, 1));
                }
                b"office:text" => in_text = true,
                b"text:h" if in_text => {
                    let style = attr_val(&e, b"text:style-name");
                    let lvl = outline_level(&e).or_else(|| style.as_deref().and_then(|s| reg.heading_level(s))).unwrap_or(1).clamp(1, 6);
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "heading");
                    doc.set_attr(&id, "level", lvl);
                    if let Some(a) = style.as_deref().and_then(|s| reg.para_align(s)) {
                        doc.set_attr(&id, "align", a.as_str());
                    }
                    cur_para_marks.clear();
                }
                b"text:p" if in_text && !in_item && table_id.is_none() => {
                    let style = attr_val(&e, b"text:style-name");
                    cur_para_marks = open_para(doc, &mut prev, &mut cur, &mut cur_len, reg, style.as_deref());
                }
                b"text:list" if in_text => {
                    // A nested `text:list` often omits its own style-name, continuing
                    // the parent list's style at the deeper level — so inherit the
                    // parent's ordered-ness instead of defaulting to bulleted.
                    let style = attr_val(&e, b"text:style-name");
                    let ordered = style
                        .as_deref()
                        .and_then(|s| reg.list_ordered.get(s).copied())
                        .unwrap_or_else(|| *list_stack.last().unwrap_or(&false));
                    list_stack.push(ordered);
                    // Numbering fidelity: format + start from the list-style; the
                    // list's own `text:start-value` overrides the style start, and
                    // `text:continue-numbering="true"` means this list continues
                    // (no restart) rather than restarting its count.
                    let numbering = style.as_deref().and_then(|s| reg.list_numbering.get(s));
                    let mut num_format = numbering.and_then(|n| n.num_format);
                    let mut num_start = numbering.and_then(|n| n.num_start);
                    if let Some(s) = attr_val(&e, b"text:start-value").and_then(|v| v.parse::<i64>().ok()) {
                        num_start = Some(s);
                    }
                    let continuing = attr_val(&e, b"text:continue-numbering")
                        .map(|v| v == "true" || v == "1")
                        .unwrap_or(false);
                    // A list that doesn't continue restarts; an explicit start-value
                    // also implies a restart at that value.
                    let restart = !continuing || num_start.is_some();
                    if ordered && num_format.is_none() {
                        num_format = Some("decimal");
                    }
                    list_num_stack.push((num_format, num_start, restart));
                }
                b"text:list-item" if in_text => {
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "list_item");
                    let ordered = *list_stack.last().unwrap_or(&false);
                    doc.set_attr(&id, "ordered", ordered);
                    // 0-based nesting depth (each open `text:list` is one level).
                    // Match the markdown codec: only emit `level` when nested, so a
                    // top-level item's attr set stays identical across codecs.
                    let level = (list_stack.len() as i64 - 1).max(0);
                    if level > 0 {
                        doc.set_attr(&id, "level", level);
                    }
                    // Numbering fidelity preservation (ordered lists only): carry the
                    // format + start + restart so the list re-exports with its
                    // numbering intact. A plain decimal-from-1 list adds no extra
                    // attrs, so the cross-codec attr set stays minimal.
                    if ordered {
                        if let Some((fmt, start, restart)) = list_num_stack.last().copied() {
                            if let Some(f) = fmt {
                                if f != "decimal" {
                                    doc.set_attr(&id, "num_format", f);
                                }
                            }
                            if let Some(s) = start {
                                if s != 1 {
                                    doc.set_attr(&id, "num_start", s);
                                    if restart {
                                        doc.set_attr(&id, "num_restart", true);
                                    }
                                }
                            }
                        }
                    }
                    in_item = true;
                }
                b"text:p" if in_text && in_item => {
                    if let Some(id) = cur.clone() {
                        if let Some(a) = attr_val(&e, b"text:style-name").as_deref().and_then(|s| reg.para_align(s)) {
                            doc.set_attr(&id, "align", a.as_str());
                        }
                    }
                }
                b"text:p" if in_text && table_id.is_some() => {
                    // The paragraph inside a table cell: lift its alignment onto the
                    // cell block so centred/right-aligned cells round-trip.
                    if let Some(id) = cur.clone() {
                        if let Some(a) = attr_val(&e, b"text:style-name").as_deref().and_then(|s| reg.para_align(s)) {
                            doc.set_attr(&id, "align", a.as_str());
                        }
                    }
                }
                b"table:table" if in_text => {
                    table_id = Some(attr_val(&e, b"table:name").unwrap_or_else(|| {
                        table_seq += 1;
                        format!("t{table_seq}")
                    }));
                    trow = 0;
                }
                b"table:table-row" if in_text => tcol = 0,
                b"table:table-cell" if in_text => {
                    if let Some(tid) = table_id.clone() {
                        let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "cell");
                        doc.set_attr(&id, "table", tid.as_str());
                        doc.set_attr(&id, "row", trow as i64);
                        doc.set_attr(&id, "col", tcol as i64);
                    }
                }
                b"text:span" if in_text => {
                    let marks = attr_val(&e, b"text:style-name").map(|s| reg.text_marks(&s)).unwrap_or_default();
                    span_stack.push((marks, cur_len));
                }
                b"text:a" if in_text => {
                    // A hyperlink: model the href as an inline `link` mark over the
                    // linked range (so it merges with concurrent text edits), plus
                    // any text marks the link's own style carries (underline/colour).
                    let href = attr_val(&e, b"xlink:href");
                    let marks = attr_val(&e, b"text:style-name").map(|s| reg.text_marks(&s)).unwrap_or_default();
                    link_stack.push((href, marks, cur_len));
                }
                b"draw:frame" if in_text && table_id.is_none() => {
                    // An image frame — either top-level or nested inside a `text:p`
                    // (the common LibreOffice/pandoc figure layout). Capture its
                    // size now, resolve the picture bytes / alt as we descend,
                    // finalize on close into an `image` block. A frame inside a
                    // paragraph records that paragraph so an empty image-only
                    // wrapper can be dropped.
                    let (enclosing_para, para_had_text) = if cur.is_some() && !in_item {
                        (cur.clone(), cur_len > 0)
                    } else {
                        (None, false)
                    };
                    // `text:anchor-type`: `as-char` → inline; `page` → page; everything
                    // else (`paragraph`/`char`/`frame`) → paragraph-anchored floating.
                    let anchor = attr_val(&e, b"text:anchor-type").map(|a| match a.as_str() {
                        "as-char" => "inline".to_string(),
                        "page" => "page".to_string(),
                        _ => "paragraph".to_string(),
                    });
                    // Wrap rides on the frame's graphic style (`draw:style-name`).
                    let wrap = attr_val(&e, b"draw:style-name").and_then(|s| reg.graphic_wrap.get(&s).cloned());
                    frame = Some(FrameState {
                        raw_start: pos_before,
                        depth: 1,
                        width: attr_val(&e, b"svg:width"),
                        height: attr_val(&e, b"svg:height"),
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
                _ if in_text && cur.is_none() && !in_item && table_id.is_none() => {
                    capture = Some((pos_before, 1));
                }
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"text:p" if in_text && !in_item && table_id.is_none() => {
                    open_para(doc, &mut prev, &mut cur, &mut cur_len, reg, attr_val(&e, b"text:style-name").as_deref());
                    cur = None;
                }
                b"text:h" if in_text => {
                    let style = attr_val(&e, b"text:style-name");
                    let lvl = outline_level(&e).or_else(|| style.as_deref().and_then(|s| reg.heading_level(s))).unwrap_or(1).clamp(1, 6);
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "heading");
                    doc.set_attr(&id, "level", lvl);
                    cur = None;
                }
                b"office:annotation-end" if in_text => {
                    // Closes a comment's range: match by `office:name` to the open
                    // annotation and anchor a `comment` mark over `[start, cur_len)`.
                    if let Some(name) = attr_val(&e, b"office:name") {
                        if let Some((cid, b, start)) = open_comments.remove(&name) {
                            comment_ranges.push((b, start, cur_len, cid));
                        }
                    }
                }
                b"text:bookmark-start" if in_text && cur.is_some() => {
                    // Opens a bookmark's range: remember the current char index; the
                    // matching `text:bookmark-end` (by `text:name`) records the end.
                    if let (Some(name), Some(b)) = (attr_val(&e, b"text:name"), cur.clone()) {
                        open_bookmarks.insert(name, (b, cur_len));
                    }
                }
                b"text:bookmark-end" if in_text => {
                    // Closes a bookmark's range: anchor a `bookmark` mark valued with the
                    // name over `[start, cur_len)`.
                    if let Some(name) = attr_val(&e, b"text:name") {
                        if let Some((b, start)) = open_bookmarks.remove(&name) {
                            bookmark_ranges.push((b, start, cur_len, name));
                        }
                    }
                }
                b"text:bookmark" if in_text && cur.is_some() => {
                    // A point bookmark (no range): anchor a zero-width range at the
                    // current char index — widened to one character on apply below.
                    if let (Some(name), Some(b)) = (attr_val(&e, b"text:name"), cur.clone()) {
                        bookmark_ranges.push((b, cur_len, cur_len, name));
                    }
                }
                b"text:change-start" if in_text && cur.is_some() => {
                    // Opens a tracked-change range: remember the current char index; the
                    // matching `text:change-end` records the end.
                    if let Some(id) = super::changes::marker_change_id(&e) {
                        if let (Some((cid, kind)), Some(b)) = (change_decls.get(&id).cloned(), cur.clone()) {
                            open_changes.insert(id, (cid, kind, b, cur_len));
                        }
                    }
                }
                b"text:change-end" if in_text => {
                    // Closes a tracked-change range: anchor an `ins`/`del` mark over
                    // `[start, cur_len)` for the matching change id.
                    if let Some(id) = super::changes::marker_change_id(&e) {
                        if let Some((cid, kind, b, start)) = open_changes.remove(&id) {
                            change_ranges.push((b, start, cur_len, kind, cid));
                        }
                    }
                }
                b"text:change" if in_text && cur.is_some() => {
                    // A point change marker (a deletion with no retained body text, or a
                    // collapsed insertion). Anchor a 1-char range if there's a character
                    // after it (widened on apply), else a zero-width range that drops.
                    if let Some(id) = super::changes::marker_change_id(&e) {
                        if let (Some((cid, kind)), Some(b)) = (change_decls.get(&id).cloned(), cur.clone()) {
                            change_ranges.push((b, cur_len, cur_len, kind, cid));
                        }
                    }
                }
                b"text:tab" if in_text => append(doc, &cur, &mut cur_len, "\t"),
                b"text:line-break" if in_text => append(doc, &cur, &mut cur_len, "\n"),
                b"text:s" if in_text => {
                    let n = attr_val(&e, b"text:c").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).min(256);
                    append(doc, &cur, &mut cur_len, &" ".repeat(n));
                }
                b"table:table-cell" if in_text => {
                    if let Some(tid) = table_id.clone() {
                        let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "cell");
                        doc.set_attr(&id, "table", tid.as_str());
                        doc.set_attr(&id, "row", trow as i64);
                        doc.set_attr(&id, "col", tcol as i64);
                        cur = None;
                    }
                    tcol += 1;
                }
                _ if in_text && cur.is_none() && !in_item && table_id.is_none() => {
                    let raw = s[pos_before..reader.buffer_position() as usize].to_string();
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "fidelity");
                    doc.set_attr(&id, "xml", raw.as_str());
                    cur = None;
                }
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"office:text" => in_text = false,
                b"text:h" => {
                    cur = None;
                }
                b"text:p" if !in_item && table_id.is_none() => {
                    flush_para_marks!();
                    cur = None;
                }
                b"text:list" => {
                    list_stack.pop();
                    list_num_stack.pop();
                }
                b"table:table" => {
                    table_id = None;
                    cur = None;
                }
                b"table:table-row" => trow += 1,
                b"table:table-cell" => {
                    cur = None;
                    tcol += 1;
                }
                b"text:list-item" => {
                    flush_para_marks!();
                    cur = None;
                    in_item = false;
                }
                b"text:span" if in_text => {
                    if let Some((marks, start)) = span_stack.pop() {
                        if let Some(b) = cur.as_ref() {
                            for (name, value) in marks {
                                pending_marks.push((b.clone(), start, cur_len, name, value));
                            }
                        }
                    }
                }
                b"text:a" if in_text => {
                    if let Some((href, marks, start)) = link_stack.pop() {
                        if let Some(b) = cur.as_ref() {
                            if let Some(h) = href {
                                pending_marks.push((b.clone(), start, cur_len, "link".to_string(), MarkValue::Str(h)));
                            }
                            for (name, value) in marks {
                                pending_marks.push((b.clone(), start, cur_len, name, value));
                            }
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some(b) = cur.as_ref() {
                    let txt = t.unescape().map_err(|e| CodecError::Parse(format!("text: {e}")))?;
                    // Skip pretty-printer indentation between structural elements
                    // (e.g. the newlines LibreOffice/pandoc insert around nested
                    // `text:list-item`s) — otherwise list items / headings import
                    // with blank leading/trailing lines and huge vertical gaps.
                    if !txt.is_empty() && !is_layout_whitespace(&txt) {
                        doc.splice_text(b, cur_len, 0, &txt);
                        cur_len += txt.chars().count();
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

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
    // Note-reference markers: cover the placeholder spliced when each `text:note`
    // opened with a `note-ref` mark valued with the note id.
    for (b, idx, nid) in note_refs {
        doc.add_mark(&b, idx, idx + 1, MARK_NOTE_REF, nid.as_str());
    }
    // Any annotation whose `office:annotation-end` never arrived (a point comment, or
    // a malformed range) still anchors at its start: extend to the block's end so the
    // comment has a non-empty range when content follows, else leave it where it sits.
    for (name, (cid, b, start)) in std::mem::take(&mut open_comments) {
        let _ = name;
        let end = doc_block_len(doc, &b).max(start);
        comment_ranges.push((b, start, end, cid));
    }
    // Comment anchor marks: cover the commented range with a `comment` mark valued
    // with the comment id. A zero-width range can't carry a mark, so widen it to one
    // character when the block has text after the anchor.
    for (b, start, mut end, cid) in comment_ranges {
        if end <= start {
            let blen = doc_block_len(doc, &b);
            end = (start + 1).min(blen);
        }
        if end > start {
            doc.add_mark(&b, start, end, MARK_COMMENT, cid.as_str());
        }
    }
    // Any bookmark whose `text:bookmark-end` never arrived still anchors at its start,
    // extending to the block's end so it has a non-empty range when content follows.
    for (name, (b, start)) in std::mem::take(&mut open_bookmarks) {
        let end = doc_block_len(doc, &b).max(start);
        bookmark_ranges.push((b, start, end, name));
    }
    // Bookmark anchor marks: cover each bookmarked range with a `bookmark` mark valued
    // with the name. A zero-width (point) range can't carry a mark, so widen it to one
    // character when the block has text after the anchor; an end-of-block point sits
    // over the last character.
    for (b, start, end, name) in bookmark_ranges {
        doc.add_bookmark(&b, start, end, &name);
    }
    // Any tracked change whose `text:change-end` never arrived still anchors at its
    // start, extending to the block's end so it has a non-empty range when content
    // follows.
    for (_id, (cid, kind, b, start)) in std::mem::take(&mut open_changes) {
        let end = doc_block_len(doc, &b).max(start);
        change_ranges.push((b, start, end, kind, cid));
    }
    // Tracked-change anchor marks: cover each changed range with the `ins`/`del` mark
    // matching the change's kind, valued with the change id. A zero-width range can't
    // carry a mark, so widen it to one character when the block has text after it.
    for (b, start, mut end, kind, cid) in change_ranges {
        if end <= start {
            let blen = doc_block_len(doc, &b);
            end = (start + 1).min(blen);
        }
        if end > start {
            doc.add_change_mark(kind, &b, start, end, &cid);
        }
    }
    Ok(())
}

/// The character length of a body block's text (0 if absent).
fn doc_block_len(doc: &DocMut, block_id: &str) -> usize {
    crate::model::block_text(doc.doc, doc.c, block_id).map(|t| t.chars().count()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::super::container::{minimal_odt, minimal_odt_styled};
    use super::super::OdtCodec;
    use crate::{AttrVal, Document, FormatProvider};

    #[test]
    fn imports_headings_paragraphs_lists() {
        let body = concat!(
            r#"<text:h text:outline-level="2">Title</text:h>"#,
            r#"<text:p>Hello</text:p>"#,
            r#"<text:list><text:list-item><text:p>one</text:p></text:list-item>"#,
            r#"<text:list-item><text:p>two</text:p></text:list-item></text:list>"#,
        );
        let odt = minimal_odt(body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();

        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Title");
        assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(2)));
        assert_eq!(snap.blocks[1].kind, "paragraph");
        assert_eq!(snap.blocks[1].text, "Hello");
        assert_eq!(snap.blocks.iter().filter(|b| b.kind == "list_item").count(), 2);
        assert_eq!(snap.blocks[2].text, "one");
        assert_eq!(snap.blocks[3].text, "two");
    }

    #[test]
    fn imports_spans_as_marks() {
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
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let p = &snap.blocks[0];
        assert_eq!(p.text, "a bold and em");
        assert!(
            p.marks.iter().any(|m| m.name == "strong" && m.start == 2 && m.end == 6),
            "strong over 'bold', got {:?}",
            p.marks
        );
        assert!(
            p.marks.iter().any(|m| m.name == "em" && m.start == 11 && m.end == 13),
            "em over 'em', got {:?}",
            p.marks
        );
    }

    #[test]
    fn imports_paragraph_align_and_inline_font() {
        use crate::document::MarkValue;
        // Alignment comes from the paragraph style; font from a text span.
        let styles = concat!(
            r#"<style:style style:name="P2" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>"#,
            r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-size="20pt" style:font-name="Liberation Serif"/></style:style>"#,
        );
        let body = r#"<text:p text:style-name="P2"><text:span text:style-name="T1">big</text:span></text:p>"#;
        let odt = minimal_odt_styled(styles, body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.text, "big");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("center".into())), "{:?}", b.attrs);
        assert!(b.marks.iter().any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20)), "{:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())), "{:?}", b.marks);
    }

    #[test]
    fn imports_named_heading_style_whitespace_and_ordered_list() {
        // The kind of structure LibreOffice emits: headings via named paragraph
        // styles, numbered lists via a list-style, explicit breaks/tabs.
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
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();

        assert_eq!(snap.blocks[0].kind, "heading", "named paragraph style → heading: {snap:?}");
        assert_eq!(snap.blocks[0].text, "My Title");
        assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(1)));
        assert_eq!(snap.blocks[1].kind, "heading", "heading via inherited parent style");
        assert!(snap.blocks[2].text.contains('\n'), "line-break: {:?}", snap.blocks[2].text);
        assert!(snap.blocks[2].text.contains('\t'), "tab: {:?}", snap.blocks[2].text);
        assert!(snap.blocks[2].text.contains("and   spaces"), "text:s spaces: {:?}", snap.blocks[2].text);
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].attrs.get("ordered"), Some(&AttrVal::Bool(true)), "numbered list-style → ordered");
    }

    #[test]
    fn table_roundtrips_as_editable_cells() {
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
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.len(), 4, "2x2 cells: {snap:?}");
        assert_eq!(cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(), vec!["a", "b", "c", "d"]);
        assert_eq!(cells[0].attrs.get("table"), Some(&AttrVal::Str("T1".into())));
        assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
        assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));

        // Round-trip: export then re-import → still a 2x2 table with the same text.
        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let cells2: Vec<_> = d2.materialize().blocks.iter().filter(|b| b.kind == "cell").map(|b| b.text.clone()).collect();
        assert_eq!(cells2, vec!["a", "b", "c", "d"], "table survives export+import");
    }

    #[test]
    fn imports_hyperlink_as_link_mark() {
        use crate::document::MarkValue;
        // A `text:a` becomes an inline `link` mark carrying the href, so it merges
        // with concurrent text edits instead of being atomized.
        let body = r#"<text:p>see <text:a xlink:href="https://qos.dev/x">here</text:a>!</text:p>"#;
        let odt = minimal_odt(body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.text, "see here!");
        let link = b.marks.iter().find(|m| m.name == "link").expect("link mark present");
        assert_eq!((link.start, link.end), (4, 8), "link over 'here': {:?}", b.marks);
        assert_eq!(link.value, MarkValue::Str("https://qos.dev/x".into()));
    }

    #[test]
    fn imports_link_wrapping_styled_span() {
        use crate::document::MarkValue;
        // A link whose inner text is also bold: link mark + strong both survive.
        let styles = r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>"#;
        let body = r#"<text:p><text:a xlink:href="http://x/"><text:span text:style-name="T1">bold</text:span></text:a></text:p>"#;
        let odt = minimal_odt_styled(styles, body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.text, "bold");
        assert!(b.marks.iter().any(|m| m.name == "link" && m.value == MarkValue::Str("http://x/".into())), "{:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "strong"), "{:?}", b.marks);
    }

    #[test]
    fn imports_text_color_as_mark() {
        use crate::document::MarkValue;
        let styles = r##"<style:style style:name="C1" style:family="text"><style:text-properties fo:color="#FF0000"/></style:style>"##;
        let body = r#"<text:p>a <text:span text:style-name="C1">red</text:span></text:p>"#;
        let odt = minimal_odt_styled(styles, body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.text, "a red");
        assert!(
            b.marks.iter().any(|m| m.name == "color" && m.start == 2 && m.end == 5 && m.value == MarkValue::Str("#ff0000".into())),
            "color mark normalized + ranged: {:?}",
            b.marks
        );
    }

    #[test]
    fn cell_alignment_roundtrips() {
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
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let cells: Vec<_> = d.materialize().blocks.into_iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].attrs.get("align"), Some(&AttrVal::Str("center".into())), "centred cell: {:?}", cells[0].attrs);
        assert_eq!(cells[1].attrs.get("align"), None, "plain cell has no align");

        // Round-trip: export then re-import → the centred cell stays centred.
        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let aligns: Vec<_> = d2.materialize().blocks.iter().filter(|b| b.kind == "cell").map(|b| b.attrs.get("align").cloned()).collect();
        assert_eq!(aligns, vec![Some(AttrVal::Str("center".into())), None], "cell alignment round-trips");
    }

    /// A valid 1x1 truecolor PNG so the in-paragraph image resolves to a real
    /// `image` block (a missing picture would fall back to a fidelity atom).
    const TINY_PNG: [u8; 69] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
        0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
        0x00, 0x03, 0x01, 0x01, 0x00, 0xf7, 0x03, 0x41, 0x43, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
        0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn imports_image_nested_in_paragraph() {
        use super::super::container::OdtContainer;
        // The common LibreOffice/pandoc figure layout: a `draw:frame` wrapped in a
        // `text:p`. The picture must become an `image` block, and the empty wrapper
        // paragraph must be dropped (no blank line before the image).
        let body = concat!(
            r#"<text:p>before</text:p>"#,
            r#"<text:p text:style-name="Fig">"#,
            r#"<draw:frame svg:width="200pt" svg:height="110pt"><draw:image xlink:href="Pictures/0.png"/></draw:frame>"#,
            r#"</text:p>"#,
            r#"<text:p>after</text:p>"#,
        );
        let base = minimal_odt_styled("", body);
        let mut c = OdtContainer::read(&base).unwrap();
        c.set("Pictures/0.png", TINY_PNG.to_vec());
        let odt = c.write().unwrap();

        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();

        assert_eq!(
            snap.blocks.iter().map(|b| b.kind.as_str()).collect::<Vec<_>>(),
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
        assert_eq!(snap.blobs.get(&hash).map(|b| b.as_slice()), Some(&TINY_PNG[..]), "blob bytes stored");
        // svg:width="200pt" → 200*96/72 ≈ 267px.
        assert_eq!(img.attrs.get("width"), Some(&AttrVal::Int(267)), "{:?}", img.attrs);
    }

    #[test]
    fn keeps_paragraph_text_before_inline_image() {
        use super::super::container::OdtContainer;
        // Text preceding an inline image stays as its own paragraph; the image is a
        // separate block right after it.
        let body = concat!(
            r#"<text:p>see <draw:frame><draw:image xlink:href="Pictures/0.png"/></draw:frame></text:p>"#,
        );
        let base = minimal_odt_styled("", body);
        let mut c = OdtContainer::read(&base).unwrap();
        c.set("Pictures/0.png", TINY_PNG.to_vec());
        let odt = c.write().unwrap();

        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        assert_eq!(snap.blocks[0].kind, "paragraph");
        assert_eq!(snap.blocks[0].text, "see ", "leading text kept: {snap:?}");
        assert_eq!(snap.blocks[1].kind, "image");
    }

    #[test]
    fn nested_list_items_capture_level() {
        // Nested `text:list`s give each item a 0-based level; top-level items carry
        // no `level` attr (matching the markdown codec), nested ones do.
        let body = concat!(
            r#"<text:list><text:list-item><text:p>a</text:p></text:list-item>"#,
            r#"<text:list-item><text:p>b</text:p>"#,
            r#"<text:list><text:list-item><text:p>nested</text:p></text:list-item></text:list>"#,
            r#"</text:list-item></text:list>"#,
        );
        let odt = minimal_odt(body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").collect();
        let level = |b: &crate::snapshot::BlockView| match b.attrs.get("level") {
            Some(AttrVal::Int(n)) => *n,
            _ => 0,
        };
        assert_eq!(
            items.iter().map(|b| (b.text.clone(), level(b))).collect::<Vec<_>>(),
            vec![("a".into(), 0i64), ("b".into(), 0), ("nested".into(), 1)],
            "nesting depth captured: {items:?}"
        );
        assert_eq!(items[0].attrs.get("level"), None, "top-level item has no level attr");
    }

    #[test]
    fn list_items_import_tight_without_layout_whitespace() {
        // Pretty-printer indentation (newlines between structural list elements)
        // must not leak into the item text as blank leading/trailing lines.
        let body = "<text:list>\n  <text:list-item>\n    <text:p>a</text:p>\n  </text:list-item>\n  <text:list-item>\n    <text:p>b</text:p>\n  </text:list-item>\n</text:list>";
        let odt = minimal_odt(body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").map(|b| b.text.clone()).collect();
        assert_eq!(items, vec!["a", "b"], "no indentation whitespace in item text: {items:?}");
    }

    #[test]
    fn preserves_unmodeled_body_element_as_fidelity_atom() {
        let body = concat!(
            r#"<text:p>before</text:p>"#,
            r#"<draw:frame draw:name="F"><draw:image xlink:href="pic.png"/></draw:frame>"#,
            r#"<text:p>after</text:p>"#,
        );
        let odt = minimal_odt_styled("", body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
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
            raw1.starts_with("<draw:frame") && raw1.contains("pic.png") && raw1.ends_with("</draw:frame>"),
            "captured raw: {raw1}"
        );

        // Round-trip: export then re-import → the table atom survives verbatim.
        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap2 = d2.materialize();
        assert_eq!(snap2.blocks.len(), 3);
        let raw2 = match snap2.blocks[1].attrs.get("xml") {
            Some(AttrVal::Str(x)) => x.clone(),
            other => panic!("fidelity atom missing after round-trip: {other:?}"),
        };
        assert_eq!(raw1, raw2, "fidelity atom round-trips verbatim");
    }
}
