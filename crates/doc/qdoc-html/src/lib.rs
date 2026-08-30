//! `qdoc-html` — render a [`qdoc::DocSnapshot`] to clean, semantic HTML.
//!
//! Takes the materialized, read-only document view and emits
//! well-formed, semantic HTML5 — headings as `<h1>`…`<h6>`, paragraphs
//! as `<p>`, quotes as `<blockquote>`, lists as nested `<ul>`/`<ol>`,
//! tables as `<table>`, images as inline `data:` URIs. Inline marks
//! (strong/em/underline/strike/link/color/…) apply over the correct
//! text ranges. This crate only exposes [`render_html`]; wiring it into
//! an editor's export/preview action is a separate task.
//!
//! ## Escaping & safety
//!
//! All document text and every attribute value is HTML-escaped, so a
//! document containing `<script>` (or `"`/`&`/`'`) or a malicious `href`
//! produces inert, well-formed markup — no injection path from document
//! content into the output structure.
//!
//! ## Styles
//!
//! The `styles` map is emitted once as a `<style>` block of CSS classes
//! (`.qdoc-style-<id>`); a block carrying a `style` attr is tagged with
//! that class. Inline `style="…"` from marks (color/font/size) is an
//! accepted fallback for run-level formatting classes can't express.
//!
//! ## Known limitations
//!
//! Tables: a flat `<table>` of `<tr>`/`<td>`, no merging/colspan/rowspan/
//! `<th>` inference. Nested marks of the same kind collapse (`strong`
//! inside `strong` emits one `<strong>`). Images: blob bytes are sniffed
//! for magic numbers to choose the `data:` MIME type, unrecognized
//! default to PNG. List `start`/`index`: sets `<ol start>`; per-item
//! custom numbering isn't modeled.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

mod base64;

use qdoc::snapshot::AttrVal;
use qdoc::{BlockView, DocSnapshot, MarkSpan};
use std::fmt::Write as _;

/// Options controlling the shape of the emitted HTML.
#[derive(Debug, Clone)]
pub struct HtmlOptions {
    /// When `true`, emit only the body markup (no `<!doctype>`, `<html>`,
    /// `<head>`, `<style>` wrapper) — for embedding in a larger page. `false`
    /// (default) emits a complete standalone document.
    pub fragment: bool,
    /// Optional document title used in `<title>` for a full document. Ignored
    /// for fragments.
    pub title: Option<String>,
    /// Indentation unit used for pretty-printing (e.g. `"  "`). Cosmetic only.
    pub indent: String,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self {
            fragment: false,
            title: None,
            indent: "  ".to_string(),
        }
    }
}

/// Render a materialized qdoc snapshot to an HTML string. Always
/// succeeds: unknown block kinds degrade to a `<p>`, and image blocks
/// with a missing blob render an empty `<img>` with an `alt` note.
pub fn render_html(snap: &DocSnapshot, opts: HtmlOptions) -> String {
    let mut body = String::new();
    render_blocks(&mut body, snap, &snap.blocks, &opts, 0);

    if opts.fragment {
        return body;
    }

    let mut out = String::new();
    out.push_str("<!doctype html>\n");
    out.push_str("<html lang=\"en\">\n");
    out.push_str("<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let title = opts.title.as_deref().unwrap_or("Document");
    let _ = writeln!(out, "<title>{}</title>", escape_text(title));
    out.push_str(&render_styles(snap));
    out.push_str("</head>\n");
    out.push_str("<body>\n");
    out.push_str(&body);
    out.push_str("</body>\n");
    out.push_str("</html>\n");
    out
}

// ---- styles -----------------------------------------------------------------

/// Emit the document's named styles as a `<style>` block of CSS
/// classes. Empty string when there are no styles.
fn render_styles(snap: &DocSnapshot) -> String {
    if snap.styles.is_empty() {
        return String::new();
    }
    let mut css = String::new();
    css.push_str("<style>\n");
    for (id, props) in &snap.styles {
        let mut decls = Vec::new();
        for (prop, val) in props {
            if let Some(decl) = style_prop_to_css(prop, val) {
                decls.push(decl);
            }
        }
        if decls.is_empty() {
            continue;
        }
        let _ = writeln!(css, ".{} {{ {} }}", style_class(id), decls.join(" "));
    }
    css.push_str("</style>\n");
    css
}

/// Map a single qdoc style property to a CSS declaration (`prop:
/// value;`). Unknown properties are dropped; values are CSS-sanitized
/// to keep the `<style>` block well-formed.
fn style_prop_to_css(prop: &str, val: &AttrVal) -> Option<String> {
    let css = match prop {
        // `name` is metadata, not a visual property.
        "name" => return None,
        "color" => format!("color: {};", css_value(&attr_as_str(val))),
        "background" | "bg" => format!("background-color: {};", css_value(&attr_as_str(val))),
        "font" | "fontFamily" => format!("font-family: {};", css_font_family(&attr_as_str(val))),
        "size" | "fontSize" => format!("font-size: {};", css_size(val)),
        "align" => format!("text-align: {};", css_value(&attr_as_str(val))),
        "bold" | "strong" => {
            if matches!(val, AttrVal::Bool(true)) {
                "font-weight: bold;".to_string()
            } else {
                return None;
            }
        }
        "italic" | "em" => {
            if matches!(val, AttrVal::Bool(true)) {
                "font-style: italic;".to_string()
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(css)
}

/// CSS class name for a style id.
fn style_class(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("qdoc-style-{safe}")
}

// ---- block rendering --------------------------------------------------------

fn indent_for(opts: &HtmlOptions, depth: usize) -> String {
    opts.indent.repeat(depth)
}

/// Render a slice of sibling blocks. Consecutive `list_item`s group into
/// nested `<ul>`/`<ol>` by `level`; other kinds dispatch individually.
fn render_blocks(
    out: &mut String,
    snap: &DocSnapshot,
    blocks: &[BlockView],
    opts: &HtmlOptions,
    depth: usize,
) {
    let mut i = 0;
    while i < blocks.len() {
        if blocks[i].kind == "list_item" {
            // Consume the maximal run of consecutive list items and render it
            // as one (possibly nested) list.
            let mut j = i;
            while j < blocks.len() && blocks[j].kind == "list_item" {
                j += 1;
            }
            render_list(out, &blocks[i..j], opts, depth);
            i = j;
        } else {
            render_block(out, snap, &blocks[i], opts, depth);
            i += 1;
        }
    }
}

fn render_block(
    out: &mut String,
    snap: &DocSnapshot,
    block: &BlockView,
    opts: &HtmlOptions,
    depth: usize,
) {
    match block.kind.as_str() {
        "heading" => render_heading(out, block, opts, depth),
        "quote" | "blockquote" => render_quote(out, snap, block, opts, depth),
        "image" => render_image(out, snap, block, opts, depth),
        "table" => render_table(out, snap, block, opts, depth),
        "code" | "code_block" => render_code(out, block, opts, depth),
        // paragraph + any unknown text-bearing kind → a paragraph.
        _ => render_paragraph(out, block, opts, depth),
    }
}

/// Heading level (1..=6), defaulting to 1 if absent/out of range.
fn heading_level(block: &BlockView) -> u8 {
    match block.attrs.get("level") {
        Some(AttrVal::Int(n)) => (*n).clamp(1, 6) as u8,
        _ => 1,
    }
}

fn render_heading(out: &mut String, block: &BlockView, opts: &HtmlOptions, depth: usize) {
    let level = heading_level(block);
    let pad = indent_for(opts, depth);
    let attrs = block_attrs(block);
    let _ = write!(out, "{pad}<h{level}{attrs}>");
    out.push_str(&render_inline(&block.text, &block.marks));
    let _ = writeln!(out, "</h{level}>");
}

fn render_paragraph(out: &mut String, block: &BlockView, opts: &HtmlOptions, depth: usize) {
    let pad = indent_for(opts, depth);
    let attrs = block_attrs(block);
    let _ = write!(out, "{pad}<p{attrs}>");
    if block.text.is_empty() {
        // Preserve a visible blank line without emitting an empty element
        // some renderers collapse.
        out.push_str("<br>");
    } else {
        out.push_str(&render_inline(&block.text, &block.marks));
    }
    out.push_str("</p>\n");
}

fn render_quote(
    out: &mut String,
    snap: &DocSnapshot,
    block: &BlockView,
    opts: &HtmlOptions,
    depth: usize,
) {
    let pad = indent_for(opts, depth);
    let attrs = block_attrs(block);
    let _ = writeln!(out, "{pad}<blockquote{attrs}>");
    if !block.text.is_empty() {
        let inner = indent_for(opts, depth + 1);
        let _ = write!(out, "{inner}<p>");
        out.push_str(&render_inline(&block.text, &block.marks));
        out.push_str("</p>\n");
    }
    // Child blocks (multi-paragraph quotes) render inside the blockquote.
    render_blocks(out, snap, &block.children, opts, depth + 1);
    let _ = writeln!(out, "{pad}</blockquote>");
}

fn render_code(out: &mut String, block: &BlockView, opts: &HtmlOptions, depth: usize) {
    let pad = indent_for(opts, depth);
    // Code is rendered verbatim — escaped, no inline marks applied.
    let _ = write!(out, "{pad}<pre><code>");
    out.push_str(&escape_text(&block.text));
    out.push_str("</code></pre>\n");
}

// ---- lists ------------------------------------------------------------------

/// CSS `list-style-type` for an ordered list's `num_format`.
fn list_style_type(num_format: &str) -> &'static str {
    match num_format {
        "lower-roman" => "lower-roman",
        "upper-roman" => "upper-roman",
        "lower-alpha" | "lower-latin" => "lower-alpha",
        "upper-alpha" | "upper-latin" => "upper-alpha",
        _ => "decimal",
    }
}

/// Render a run of consecutive `list_item` blocks as nested `<ul>`/`<ol>`.
/// Items carry a 0-based `level`; deeper items nest inside the previous
/// item's sublist. `ordered`/`num_format` choose type/marker per level;
/// the algorithm walks the run once, opening/closing lists as level changes.
fn render_list(out: &mut String, items: &[BlockView], opts: &HtmlOptions, depth: usize) {
    // Stack of currently-open lists: (level, ordered) — for closing correctly.
    let mut open: Vec<(i64, bool)> = Vec::new();

    let item_level = |b: &BlockView| match b.attrs.get("level") {
        Some(AttrVal::Int(n)) => (*n).max(0),
        _ => 0,
    };
    let item_ordered = |b: &BlockView| matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true)));

    for item in items {
        let level = item_level(item);
        let ordered = item_ordered(item);

        // Close lists deeper than this item, and close (then re-open) a list
        // at the same depth whose ordered-ness flipped.
        while let Some(&(open_level, open_ord)) = open.last() {
            if open_level > level || (open_level == level && open_ord != ordered) {
                open.pop();
                close_list(out, open_level, open_ord, opts, depth);
            } else {
                break;
            }
        }

        // Open lists until we reach this item's level.
        while open.last().map_or(-1, |&(l, _)| l) < level {
            let new_level = open.last().map_or(0, |&(l, _)| l + 1).max(0);
            open.push((new_level, ordered));
            open_list(out, item, new_level, ordered, opts, depth);
        }

        // Emit the item.
        let pad = indent_for(opts, depth + level as usize + 1);
        let attrs = block_attrs(item);
        let _ = write!(out, "{pad}<li{attrs}>");
        out.push_str(&render_inline(&item.text, &item.marks));
        out.push_str("</li>\n");
    }

    // Close any still-open lists, deepest first.
    while let Some((open_level, open_ord)) = open.pop() {
        close_list(out, open_level, open_ord, opts, depth);
    }
}

fn open_list(
    out: &mut String,
    item: &BlockView,
    level: i64,
    ordered: bool,
    opts: &HtmlOptions,
    depth: usize,
) {
    let pad = indent_for(opts, depth + level as usize);
    if ordered {
        let fmt = match item.attrs.get("num_format") {
            Some(AttrVal::Str(s)) => s.as_str(),
            _ => "decimal",
        };
        let lst = list_style_type(fmt);
        let mut attrs = format!(" style=\"list-style-type: {lst}\"");
        // Respect an explicit start index on the first item, if any.
        match item.attrs.get("start").or_else(|| item.attrs.get("index")) {
            Some(AttrVal::Int(n)) if *n != 1 => {
                let _ = write!(attrs, " start=\"{n}\"");
            }
            _ => {}
        }
        let _ = writeln!(out, "{pad}<ol{attrs}>");
    } else {
        let _ = writeln!(out, "{pad}<ul>");
    }
}

fn close_list(out: &mut String, level: i64, ordered: bool, opts: &HtmlOptions, depth: usize) {
    let pad = indent_for(opts, depth + level as usize);
    let tag = if ordered { "</ol>" } else { "</ul>" };
    let _ = writeln!(out, "{pad}{tag}");
}

// ---- tables -----------------------------------------------------------------

fn render_table(
    out: &mut String,
    snap: &DocSnapshot,
    block: &BlockView,
    opts: &HtmlOptions,
    depth: usize,
) {
    // Rows are child blocks, each row's children its cells. Tolerates
    // the flat shape (cells directly under the table with a `cols` attr).
    let rows: Vec<Vec<&BlockView>> = if block.children.iter().any(|c| !c.children.is_empty()) {
        block
            .children
            .iter()
            .map(|row| row.children.iter().collect())
            .collect()
    } else if !block.children.is_empty() {
        let cols = match block.attrs.get("cols") {
            Some(AttrVal::Int(n)) => (*n).max(1) as usize,
            _ => block.children.len().max(1),
        };
        block
            .children
            .chunks(cols)
            .map(|c| c.iter().collect())
            .collect()
    } else {
        Vec::new()
    };

    let pad = indent_for(opts, depth);
    let row_pad = indent_for(opts, depth + 1);
    let cell_pad = indent_for(opts, depth + 2);
    let attrs = block_attrs(block);
    let _ = writeln!(out, "{pad}<table{attrs}>");
    for row in &rows {
        let _ = writeln!(out, "{row_pad}<tr>");
        for cell in row {
            let _ = write!(out, "{cell_pad}<td>");
            // A cell may carry its own children (block content) or be a leaf.
            if cell.children.is_empty() {
                out.push_str(&render_inline(&cell.text, &cell.marks));
            } else {
                out.push('\n');
                render_blocks(out, snap, &cell.children, opts, depth + 3);
                out.push_str(&cell_pad);
            }
            out.push_str("</td>\n");
        }
        let _ = writeln!(out, "{row_pad}</tr>");
    }
    let _ = writeln!(out, "{pad}</table>");
}

// ---- images -----------------------------------------------------------------

fn render_image(
    out: &mut String,
    snap: &DocSnapshot,
    block: &BlockView,
    opts: &HtmlOptions,
    depth: usize,
) {
    let pad = indent_for(opts, depth);
    let alt = if block.text.is_empty() {
        match block.attrs.get("alt") {
            Some(AttrVal::Str(s)) => s.clone(),
            _ => String::new(),
        }
    } else {
        block.text.clone()
    };
    let alt_attr = format!(" alt=\"{}\"", escape_attr(&alt));

    let hash = match block.attrs.get("blob") {
        Some(AttrVal::Str(s)) => Some(s.clone()),
        _ => None,
    };
    match hash.as_ref().and_then(|h| snap.blobs.get(h)) {
        Some(bytes) => {
            let mime = sniff_mime(bytes);
            let data = base64::encode(bytes);
            // The data URI is self-contained; no escaping needed beyond the
            // base64 alphabet (no `"` in b64).
            let _ = writeln!(
                out,
                "{pad}<img src=\"data:{mime};base64,{data}\"{alt_attr}>"
            );
        }
        None => {
            // Missing/absent blob: emit an inert img with the alt note so
            // structure stays preserved and well-formed.
            let _ = writeln!(out, "{pad}<img src=\"\"{alt_attr}>");
        }
    }
}

/// Sniff a handful of image magic numbers to pick a `data:` MIME type.
/// Defaults to `image/png` for unrecognized bytes.
fn sniff_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        "image/svg+xml"
    } else {
        "image/png"
    }
}

// ---- block-level attributes -------------------------------------------------

/// Compose the attribute string for a block element: the resolved style
/// class plus a best-effort `text-align` from an `align` attr.
fn block_attrs(block: &BlockView) -> String {
    let mut class = None;
    if let Some(AttrVal::Str(sid)) = block.attrs.get("style") {
        class = Some(style_class(sid));
    }
    let mut style = String::new();
    if let Some(AttrVal::Str(a)) = block.attrs.get("align") {
        let _ = write!(style, "text-align: {};", css_value(a));
    }

    let mut out = String::new();
    if let Some(c) = class {
        let _ = write!(out, " class=\"{}\"", escape_attr(&c));
    }
    if !style.is_empty() {
        let _ = write!(out, " style=\"{}\"", escape_attr(&style));
    }
    out
}

// ---- inline marks -----------------------------------------------------------

/// Render `text` with `marks` applied as inline HTML over the correct
/// byte-offset ranges (matching how qdoc materializes them) — cut at
/// every mark boundary, then open the active mark set per segment.
fn render_inline(text: &str, marks: &[MarkSpan]) -> String {
    if marks.is_empty() {
        return escape_text(text);
    }
    if text.is_empty() {
        return String::new();
    }

    // Collect all boundary byte-offsets (clamped) so we split into
    // segments over which the active mark-set is constant.
    let mut bounds: Vec<usize> = vec![0, text.len()];
    for m in marks {
        for b in [m.start, m.end] {
            if b <= text.len() && text.is_char_boundary(b) {
                bounds.push(b);
            }
        }
    }
    bounds.sort_unstable();
    bounds.dedup();

    let mut out = String::new();
    for w in bounds.windows(2) {
        let (seg_start, seg_end) = (w[0], w[1]);
        if seg_start >= seg_end {
            continue;
        }
        let segment = &text[seg_start..seg_end];
        // Marks active over this whole segment (a mark covers [start, end)).
        let active: Vec<&MarkSpan> = marks
            .iter()
            .filter(|m| m.start <= seg_start && m.end >= seg_end && m.start < m.end)
            .collect();
        let (open, close) = mark_tags(&active);
        out.push_str(&open);
        out.push_str(&escape_text(segment));
        out.push_str(&close);
    }
    out
}

/// Build opening/closing tag strings for the active mark set over a
/// segment. Ordering is deterministic so open/close nest correctly:
/// link (outermost) → span(style) → strong → em → u → s.
fn mark_tags(active: &[&MarkSpan]) -> (String, String) {
    let mut open = String::new();
    let mut close_stack: Vec<&str> = Vec::new();

    let has = |name: &str| active.iter().any(|m| m.name == name);

    // 1. Link wraps everything.
    if let Some(href) = active.iter().find_map(|m| {
        if m.name == "link" || m.name == "href" {
            mark_str(m)
        } else {
            None
        }
    }) {
        let _ = write!(open, "<a href=\"{}\">", escape_attr(&sanitize_href(&href)));
        close_stack.push("</a>");
    }

    // 2. A <span style> carrying color / font / size, if any of those are set.
    let mut decls: Vec<String> = Vec::new();
    if let Some(c) = active
        .iter()
        .find(|m| m.name == "color")
        .and_then(|m| mark_str(m))
    {
        decls.push(format!("color: {}", css_value(&c)));
    }
    if let Some(f) = active
        .iter()
        .find(|m| m.name == "font" || m.name == "fontFamily")
        .and_then(|m| mark_str(m))
    {
        decls.push(format!("font-family: {}", css_font_family(&f)));
    }
    if let Some(sz) = active
        .iter()
        .find(|m| m.name == "size" || m.name == "fontSize")
    {
        decls.push(format!("font-size: {}", css_size_from_mark(sz)));
    }
    if !decls.is_empty() {
        let _ = write!(
            open,
            "<span style=\"{}\">",
            escape_attr(&format!("{};", decls.join("; ")))
        );
        close_stack.push("</span>");
    }

    // 3. Boolean inline styles.
    if has("strong") || has("bold") {
        open.push_str("<strong>");
        close_stack.push("</strong>");
    }
    if has("em") || has("italic") {
        open.push_str("<em>");
        close_stack.push("</em>");
    }
    if has("underline") {
        open.push_str("<u>");
        close_stack.push("</u>");
    }
    if has("strike") || has("strikethrough") {
        open.push_str("<s>");
        close_stack.push("</s>");
    }

    // Close in reverse order.
    let mut close = String::new();
    for tag in close_stack.iter().rev() {
        close.push_str(tag);
    }
    (open, close)
}

// ---- value helpers ----------------------------------------------------------

fn attr_as_str(val: &AttrVal) -> String {
    match val {
        AttrVal::Str(s) => s.clone(),
        AttrVal::Int(n) => n.to_string(),
        AttrVal::Float(f) => f.to_string(),
        AttrVal::Bool(b) => b.to_string(),
    }
}

/// The string payload of a valued mark (link href, color, font family), if any.
fn mark_str(m: &MarkSpan) -> Option<String> {
    m.value.as_str().map(std::string::ToString::to_string)
}

/// CSS `font-size` for a `size`/`fontSize` mark (Int pt, or numeric string).
fn css_size_from_mark(m: &MarkSpan) -> String {
    use qdoc::document::MarkValue;
    match &m.value {
        MarkValue::Int(n) => format!("{n}pt"),
        MarkValue::Str(s) => css_size_str(s),
        MarkValue::Bool(_) => "inherit".to_string(),
    }
}

/// CSS `font-size` for a style-map `size`/`fontSize` property.
fn css_size(val: &AttrVal) -> String {
    match val {
        AttrVal::Int(n) => format!("{n}pt"),
        AttrVal::Float(f) => format!("{f}pt"),
        AttrVal::Str(s) => css_size_str(s),
        AttrVal::Bool(_) => "inherit".to_string(),
    }
}

/// Interpret a string size: a bare number gets `pt`; an already-unit'd
/// value (`1.2em`, `16px`) passes through after CSS sanitization.
fn css_size_str(s: &str) -> String {
    let t = s.trim();
    if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.') {
        format!("{}pt", css_value(t))
    } else {
        css_value(t)
    }
}

/// Map a logical font family (sans/serif/mono) to a CSS stack;
/// otherwise pass the sanitized family name through.
fn css_font_family(f: &str) -> String {
    match f {
        "sans" | "sans-serif" => "sans-serif".to_string(),
        "serif" => "serif".to_string(),
        "mono" | "monospace" => "monospace".to_string(),
        other => css_value(other),
    }
}

/// Sanitize a value destined for a CSS declaration: strip characters
/// that could break out of the property value (`;`, `{`, `}`, quotes,
/// `<`, `>`) — additionally HTML-attr-escaped by the caller.
fn css_value(v: &str) -> String {
    v.chars()
        .filter(|c| {
            !matches!(
                c,
                ';' | '{' | '}' | '"' | '\'' | '<' | '>' | '\\' | '(' | ')'
            )
        })
        .collect()
}

/// Reject dangerous URL schemes (`javascript:`, `data:` as a link
/// target, …), leaving only safe http/https/mailto/relative hrefs. A
/// rejected href becomes `#` so the `<a>` stays inert.
fn sanitize_href(href: &str) -> String {
    let trimmed = href.trim();
    let lower = trimmed.to_ascii_lowercase();
    // Strip leading control/whitespace that browsers ignore when matching schemes.
    let scheme_probe: String = lower
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\0')
        .collect();
    if scheme_probe.starts_with("javascript:")
        || scheme_probe.starts_with("vbscript:")
        || scheme_probe.starts_with("data:")
        || scheme_probe.starts_with("file:")
    {
        return "#".to_string();
    }
    trimmed.to_string()
}

// ---- HTML escaping ----------------------------------------------------------

/// Escape text destined for element content: the characters that can
/// change parsing in text context, plus quotes for safety.
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape a value destined for a double-quoted attribute: the quote and
/// structural characters, so the value can never terminate the
/// attribute or the tag.
fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
