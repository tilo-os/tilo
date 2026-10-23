use crate::resolved::Resolved;
use crate::xml_escape::{escape_html_attr, escape_text};
use base64::Engine as _;
use std::fmt::Write as _;
use tilo_content::ContentSource;
use tilo_doc::export::{BlockKind, ExportBlock, TablePlan};
use tilo_doc::snapshot::AttrVal;
use tilo_doc::{BlockView, DocSnapshot, InlinePlan, MarkSpan};

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

/// Render a materialized qdoc snapshot to an HTML string, embedding the images it references (read
/// from `content`) as `data:` URIs. Always succeeds: unknown block kinds degrade to a `<p>`, and
/// image blocks whose content is missing render an empty `<img>` with an `alt` note.
pub fn render_html(snap: &DocSnapshot, opts: HtmlOptions, content: &dyn ContentSource) -> String {
    let snap = tilo_doc::export::without_kept_placeholders(snap);
    render_resolved(&Resolved::new(&snap, content), opts)
}

/// The rich text under `root` (a rich-text field of another document) as an HTML fragment: the
/// body markup only, for a message part. `None` when `root` holds no rich text.
pub fn render_html_fragment_under(
    doc: &automerge::AutoCommit,
    root: &automerge::ObjId,
    content: &dyn ContentSource,
) -> Option<String> {
    let c = tilo_doc::model::open(doc, root).ok()?;
    let options = HtmlOptions {
        fragment: true,
        ..HtmlOptions::default()
    };
    Some(render_html(
        &tilo_doc::model::materialize(doc, &c),
        options,
        content,
    ))
}

/// [`render_html`] over a snapshot whose images are already resolved.
#[allow(
    clippy::needless_pass_by_value,
    reason = "mirrors render_html, which takes HtmlOptions by value"
)]
pub(crate) fn render_resolved(snap: &Resolved<'_>, opts: HtmlOptions) -> String {
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

/// Emit the document's named styles as a `<style>` block of CSS
/// classes. Empty string when there are no styles.
fn render_styles(snap: &Resolved<'_>) -> String {
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
        "color" => format!("color: {};", css_value(&attr_as_str(val))),
        "background" | "bg" => format!("background-color: {};", css_value(&attr_as_str(val))),
        "font" | "fontFamily" => format!("font-family: {};", css_font_family(&attr_as_str(val))),
        "size" | "fontSize" => format!("font-size: {};", css_size(val)),
        "align" => format!("text-align: {};", css_value(&attr_as_str(val))),
        "bold" | "strong" | "italic" | "em" => {
            if !matches!(val, AttrVal::Bool(true)) {
                return None;
            }
            match prop {
                "bold" | "strong" => "font-weight: bold;".to_string(),
                _ => "font-style: italic;".to_string(),
            }
        }
        // `name` is metadata, not a visual property; anything else is unrecognised.
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

fn indent_for(opts: &HtmlOptions, depth: usize) -> String {
    opts.indent.repeat(depth)
}

/// Render a slice of sibling blocks. Consecutive `list_item`s group into
/// nested `<ul>`/`<ol>` by `level`; other kinds dispatch individually.
fn render_blocks(
    out: &mut String,
    snap: &Resolved<'_>,
    blocks: &[BlockView],
    opts: &HtmlOptions,
    depth: usize,
) {
    let mut i = 0;
    while let Some(block) = blocks.get(i) {
        if ExportBlock::new(block).kind() == BlockKind::ListItem {
            // Consume the maximal run of consecutive list items and render it
            // as one (possibly nested) list.
            let mut j = i;
            while blocks
                .get(j)
                .is_some_and(|block| ExportBlock::new(block).kind() == BlockKind::ListItem)
            {
                j = j.saturating_add(1);
            }
            // `get(..)` degrades an inverted/out-of-range slice to empty
            // rather than panicking; `i..j` is always ascending and
            // in-bounds by construction, so this never actually triggers.
            render_list(out, blocks.get(i..j).unwrap_or(&[]), opts, depth);
            i = j;
        } else {
            render_block(out, snap, block, opts, depth);
            i = i.saturating_add(1);
        }
    }
}

fn render_block(
    out: &mut String,
    snap: &Resolved<'_>,
    block: &BlockView,
    opts: &HtmlOptions,
    depth: usize,
) {
    match ExportBlock::new(block).kind() {
        BlockKind::Heading => render_heading(out, block, opts, depth),
        BlockKind::Quote => render_quote(out, snap, block, opts, depth),
        BlockKind::Image => render_image(out, snap, block, opts, depth),
        BlockKind::Table => render_table(out, snap, block, opts, depth),
        BlockKind::Code => render_code(out, block, opts, depth),
        BlockKind::Paragraph | BlockKind::ListItem => render_paragraph(out, block, opts, depth),
    }
}

fn render_heading(out: &mut String, block: &BlockView, opts: &HtmlOptions, depth: usize) {
    let level = ExportBlock::new(block).heading_level();
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
    snap: &Resolved<'_>,
    block: &BlockView,
    opts: &HtmlOptions,
    depth: usize,
) {
    let pad = indent_for(opts, depth);
    let attrs = block_attrs(block);
    let _ = writeln!(out, "{pad}<blockquote{attrs}>");
    if !block.text.is_empty() {
        let inner = indent_for(opts, depth.saturating_add(1));
        let _ = write!(out, "{inner}<p>");
        out.push_str(&render_inline(&block.text, &block.marks));
        out.push_str("</p>\n");
    }
    // Child blocks (multi-paragraph quotes) render inside the blockquote.
    render_blocks(out, snap, &block.children, opts, depth.saturating_add(1));
    let _ = writeln!(out, "{pad}</blockquote>");
}

fn render_code(out: &mut String, block: &BlockView, opts: &HtmlOptions, depth: usize) {
    let pad = indent_for(opts, depth);
    // Code is rendered verbatim — escaped, no inline marks applied.
    let _ = write!(out, "{pad}<pre><code>");
    out.push_str(&escape_text(&block.text));
    out.push_str("</code></pre>\n");
}

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

    // `level` comes from the document; clamp to a nesting depth no real
    // list reaches, so a corrupt/hostile attribute can't turn the
    // indent-string repeats below into an unbounded allocation.
    for item in items {
        let level = ExportBlock::new(item).list_level().min(64);
        let ordered = ExportBlock::new(item).ordered();

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
            let new_level = open.last().map_or(0, |&(l, _)| l.saturating_add(1)).max(0);
            open.push((new_level, ordered));
            open_list(out, item, new_level, ordered, opts, depth);
        }

        // Emit the item. `level` is clamped to 0..=64 above, so this cast
        // never loses the sign or truncates.
        #[allow(
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation,
            reason = "level is clamped to 0..=64 above, so it is always non-negative and small"
        )]
        let pad = indent_for(opts, depth.saturating_add(level as usize).saturating_add(1));
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
    // `level` is always a value `render_list` already clamped to 0..=64.
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "render_list, this function's only caller, clamps level to 0..=64 before passing it"
    )]
    let pad = indent_for(opts, depth.saturating_add(level as usize));
    if ordered {
        let item = ExportBlock::new(item);
        let lst = list_style_type(item.num_format());
        let mut attrs = format!(" style=\"list-style-type: {lst}\"");
        // The first item carries where the list's numbering starts.
        let start = item.num_start();
        if start != 1 {
            let _ = write!(attrs, " start=\"{start}\"");
        }
        let _ = writeln!(out, "{pad}<ol{attrs}>");
    } else {
        let _ = writeln!(out, "{pad}<ul>");
    }
}

fn close_list(out: &mut String, level: i64, ordered: bool, opts: &HtmlOptions, depth: usize) {
    // `level` is always a value `render_list` already clamped to 0..=64.
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "render_list, this function's only caller, clamps level to 0..=64 before passing it"
    )]
    let pad = indent_for(opts, depth.saturating_add(level as usize));
    let tag = if ordered { "</ol>" } else { "</ul>" };
    let _ = writeln!(out, "{pad}{tag}");
}

fn render_table(
    out: &mut String,
    snap: &Resolved<'_>,
    block: &BlockView,
    opts: &HtmlOptions,
    depth: usize,
) {
    // Rows are child blocks, each row's children its cells. Tolerates
    // the flat shape (cells directly under the table with a `cols` attr).
    let plan = TablePlan::new(block);

    let pad = indent_for(opts, depth);
    let row_pad = indent_for(opts, depth.saturating_add(1));
    let cell_pad = indent_for(opts, depth.saturating_add(2));
    let attrs = block_attrs(block);
    let _ = writeln!(out, "{pad}<table{attrs}>");
    for row in plan.rows() {
        let _ = writeln!(out, "{row_pad}<tr>");
        for cell in row {
            let _ = write!(out, "{cell_pad}<td>");
            // A cell may carry its own children (block content) or be a leaf.
            if cell.children.is_empty() {
                out.push_str(&render_inline(&cell.text, &cell.marks));
            } else {
                out.push('\n');
                render_blocks(out, snap, &cell.children, opts, depth.saturating_add(3));
                out.push_str(&cell_pad);
            }
            out.push_str("</td>\n");
        }
        let _ = writeln!(out, "{row_pad}</tr>");
    }
    let _ = writeln!(out, "{pad}</table>");
}

fn render_image(
    out: &mut String,
    snap: &Resolved<'_>,
    block: &BlockView,
    opts: &HtmlOptions,
    depth: usize,
) {
    let pad = indent_for(opts, depth);
    let alt = if block.text.is_empty() {
        block.str_attr("alt").unwrap_or_default().to_owned()
    } else {
        block.text.clone()
    };
    let alt_attr = format!(" alt=\"{}\"", escape_html_attr(&alt));

    match block.str_attr("blob").and_then(|h| snap.image(h)) {
        Some(bytes) => {
            let mime = tilo_content::image_format(bytes).map_or("image/png", |media| media.mime);
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
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

/// Compose the attribute string for a block element: the resolved style
/// class plus a best-effort `text-align` from an `align` attr.
fn block_attrs(block: &BlockView) -> String {
    let class = if let Some(AttrVal::Str(sid)) = block.attrs.get("style") {
        Some(style_class(sid))
    } else {
        None
    };
    let mut style = String::new();
    if let Some(AttrVal::Str(a)) = block.attrs.get("align") {
        let _ = write!(style, "text-align: {};", css_value(a));
    }

    let mut out = String::new();
    if let Some(c) = class {
        let _ = write!(out, " class=\"{}\"", escape_html_attr(&c));
    }
    if !style.is_empty() {
        let _ = write!(out, " style=\"{}\"", escape_html_attr(&style));
    }
    out
}

/// Render `text` with qdoc's canonical character-offset mark traversal.
fn render_inline(text: &str, marks: &[MarkSpan]) -> String {
    if text.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    for point in InlinePlan::new(text, marks).points() {
        let Some(run) = &point.run else {
            continue;
        };
        let (open, close) = mark_tags(&run.marks);
        out.push_str(&open);
        out.push_str(&escape_text(run.text));
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
        let _ = write!(
            open,
            "<a href=\"{}\">",
            escape_html_attr(&sanitize_href(&href))
        );
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
            escape_html_attr(&format!("{};", decls.join("; ")))
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
    use tilo_doc::document::MarkValue;
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

#[cfg(test)]
mod tests;
