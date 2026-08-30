//! `qdoc-rtf` — render a [`qdoc::DocSnapshot`] to RTF (Rich Text Format).
//!
//! Rounds out qdoc's export formats alongside `qdoc-pdf` and
//! `qdoc-html`. Takes the materialized, read-only document view and
//! emits a well-formed RTF document: a `{\rtf1\ansi …}` group with a
//! font table, a color table, and a body of paragraphs. Every group is
//! balanced so the output opens cleanly in WordPad/LibreOffice/Word/TextEdit.
//!
//! ## Block mapping
//!
//! heading → bold paragraph at a larger font size keyed to level;
//! paragraph → `\par`-terminated run; quote/blockquote → indented
//! paragraph (`\li`); `list_item` → bulleted (`\bullet`) or numbered run
//! with hanging indent; table → `\trowd \cellx… <cell>\cell …\row` rows;
//! `code/code_block` → monospace paragraph, marks not applied.
//!
//! ## Inline marks
//!
//! Marks split text into runs over which the active set is constant
//! (same boundary-splitting approach as `qdoc-html`). Each run emits:
//! `\b`, `\i`, `\ul`, `\strike`, `\cfN` (color), `\fN` (font), `\fsN`
//! (size, half-points). A `href`/`link` mark wraps its run in an RTF
//! `HYPERLINK` field.
//!
//! ## Escaping
//!
//! `\`, `{`, `}` are backslash-escaped; non-ASCII is emitted as `\uN?`
//! unicode escapes (signed 16-bit code unit + `?` ASCII fallback), the
//! portable RTF way to carry Unicode.
//!
//! ## Known limitations
//!
//! Tables: a flat grid of equal-width cells, no merging/colspan/rowspan/
//! header rows. Nested lists: `level` adds left-indent but no
//! `\listtable` — numbering markers are literal. Images: skipped
//! (`\pict` hex-blob embedding is out of scope); `alt` text, if any,
//! renders as a paragraph. Named styles: not emitted as an RTF
//! stylesheet; formatting resolves inline.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

use qdoc::snapshot::AttrVal;
use qdoc::{BlockView, DocSnapshot, MarkSpan};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Font-table indices. RTF references fonts by `\fN`; these are the
/// slots registered in `\fonttbl` in this fixed order.
const FONT_SANS: usize = 0;
const FONT_SERIF: usize = 1;
const FONT_MONO: usize = 2;

/// Base body font size in points. Headings scale up from here by level.
const BODY_PT: i64 = 11;

/// Twips per indent step (RTF measures indents in twips; 720 ≈ 0.5 inch).
const INDENT_TWIPS: i64 = 720;

/// Render a materialized qdoc snapshot to an RTF string. Always
/// succeeds: unknown block kinds degrade to a paragraph, image blocks
/// degrade to their alt text (or are skipped).
pub fn render_rtf(snap: &DocSnapshot) -> String {
    // Pass 1: collect the color table from every `color` mark in the document.
    let mut colors = ColorTable::new();
    collect_colors(&snap.blocks, &mut colors);

    // Pass 2: render the body into a buffer (never references colors not
    // already collected, since collection walked the same blocks).
    let mut body = String::new();
    render_blocks(&mut body, &snap.blocks, &colors, 0);

    // Assemble: header + font table + color table + body, all inside one group.
    let mut out = String::new();
    out.push_str("{\\rtf1\\ansi\\deff0");
    out.push_str(FONT_TABLE);
    out.push_str(&colors.render());
    // A leading newline keeps the control words off the header line for
    // readability; RTF ignores it.
    out.push('\n');
    out.push_str(&body);
    out.push('}');
    out
}

/// The fixed font table: sans → Helvetica (swiss), serif → Times
/// (roman), mono → Courier (modern).
const FONT_TABLE: &str = "{\\fonttbl\
{\\f0\\fswiss Helvetica;}\
{\\f1\\froman Times;}\
{\\f2\\fmodern Courier;}}";

// ---- color table ------------------------------------------------------------

/// Collects distinct RGB colors and renders the RTF `\colortbl`. RTF
/// reserves color index 0 as "auto", so registered colors start at 1;
/// `index_of` returns the 1-based `\cfN` index.
struct ColorTable {
    /// Insertion-ordered distinct colors as `(r, g, b)`.
    colors: Vec<(u8, u8, u8)>,
    /// Lookup from packed rgb → position in `colors`.
    seen: BTreeMap<(u8, u8, u8), usize>,
}

impl ColorTable {
    fn new() -> Self {
        Self {
            colors: Vec::new(),
            seen: BTreeMap::new(),
        }
    }

    /// Register a color and return its 1-based RTF color index (`\cfN`).
    fn intern(&mut self, rgb: (u8, u8, u8)) -> usize {
        if let Some(&pos) = self.seen.get(&rgb) {
            return pos + 1;
        }
        let pos = self.colors.len();
        self.colors.push(rgb);
        self.seen.insert(rgb, pos);
        pos + 1
    }

    /// The 1-based RTF color index for an already-registered color, if present.
    fn index_of(&self, rgb: (u8, u8, u8)) -> Option<usize> {
        self.seen.get(&rgb).map(|&p| p + 1)
    }

    /// Render the `{\colortbl …}` group; the leading empty entry (`;`) is
    /// the reserved "auto" color at index 0.
    fn render(&self) -> String {
        let mut s = String::from("{\\colortbl;");
        for &(r, g, b) in &self.colors {
            let _ = write!(s, "\\red{r}\\green{g}\\blue{b};");
        }
        s.push('}');
        s
    }
}

/// Walk blocks (and children) interning every `color` mark so the color
/// table is complete before the body renders.
fn collect_colors(blocks: &[BlockView], colors: &mut ColorTable) {
    for b in blocks {
        for m in &b.marks {
            if m.name == "color"
                && let Some(hex) = mark_str(m).as_deref().and_then(parse_hex_color)
            {
                colors.intern(hex);
            }
        }
        collect_colors(&b.children, colors);
    }
}

// ---- block rendering --------------------------------------------------------

/// Render a slice of sibling blocks. List items render individually
/// (each a self-contained `\par`), matching qdoc's flat run.
fn render_blocks(out: &mut String, blocks: &[BlockView], colors: &ColorTable, indent_level: i64) {
    for block in blocks {
        render_block(out, block, colors, indent_level);
    }
}

fn render_block(out: &mut String, block: &BlockView, colors: &ColorTable, indent_level: i64) {
    match block.kind.as_str() {
        "heading" => render_heading(out, block, colors),
        "quote" | "blockquote" => render_quote(out, block, colors, indent_level),
        "list_item" => render_list_item(out, block, colors),
        "table" => render_table(out, block, colors),
        "code" | "code_block" => render_code(out, block),
        "image" => render_image(out, block, indent_level),
        // paragraph + any unknown text-bearing kind → a paragraph.
        _ => render_paragraph(out, block, colors, indent_level),
    }
}

/// Heading level (1..=6), defaulting to 1 if absent/out of range.
fn heading_level(block: &BlockView) -> i64 {
    match block.attrs.get("level") {
        Some(AttrVal::Int(n)) => (*n).clamp(1, 6),
        _ => 1,
    }
}

/// Point size for a heading level: H1 ≈ 24pt down to H6 ≈ 12pt.
fn heading_pt(level: i64) -> i64 {
    match level {
        1 => 24,
        2 => 20,
        3 => 16,
        4 => 14,
        5 => 13,
        _ => 12,
    }
}

fn render_heading(out: &mut String, block: &BlockView, colors: &ColorTable) {
    let level = heading_level(block);
    let pt = heading_pt(level);
    out.push_str("{\\pard");
    push_align(out, block);
    // Bold + larger font for the whole heading paragraph.
    let _ = write!(out, "\\b\\fs{}", pt * 2);
    out.push(' ');
    render_inline(out, &block.text, &block.marks, colors);
    out.push_str("\\par}\n");
}

fn render_paragraph(out: &mut String, block: &BlockView, colors: &ColorTable, indent_level: i64) {
    out.push_str("{\\pard");
    push_indent(out, indent_level);
    push_align(out, block);
    let _ = write!(out, "\\fs{}", BODY_PT * 2);
    out.push(' ');
    render_inline(out, &block.text, &block.marks, colors);
    out.push_str("\\par}\n");
}

fn render_quote(out: &mut String, block: &BlockView, colors: &ColorTable, indent_level: i64) {
    // A quote is an indented, italic paragraph one level deeper than its context.
    out.push_str("{\\pard");
    push_indent(out, indent_level + 1);
    push_align(out, block);
    let _ = write!(out, "\\i\\fs{}", BODY_PT * 2);
    out.push(' ');
    if !block.text.is_empty() {
        render_inline(out, &block.text, &block.marks, colors);
    }
    out.push_str("\\par}\n");
    // Multi-paragraph quotes carry child blocks; render them indented too.
    render_blocks(out, &block.children, colors, indent_level + 1);
}

fn render_code(out: &mut String, block: &BlockView) {
    // Code renders verbatim in the mono font; inline marks are not applied.
    let _ = write!(out, "{{\\pard\\f{FONT_MONO}\\fs{} ", BODY_PT * 2);
    push_escaped(out, &block.text);
    out.push_str("\\par}\n");
}

/// List `level` (0-based nesting depth) → indent step count.
fn list_level(block: &BlockView) -> i64 {
    match block.attrs.get("level") {
        Some(AttrVal::Int(n)) => (*n).max(0),
        _ => 0,
    }
}

fn render_list_item(out: &mut String, block: &BlockView, colors: &ColorTable) {
    let level = list_level(block);
    let ordered = matches!(block.attrs.get("ordered"), Some(AttrVal::Bool(true)));
    out.push_str("{\\pard");
    // Hanging indent: left margin one step past the bullet at this nesting level.
    push_indent(out, level + 1);
    let _ = write!(out, "\\fi-{INDENT_TWIPS}");
    let _ = write!(out, "\\fs{}", BODY_PT * 2);
    out.push(' ');
    if ordered {
        // Numbered: the marker is literal text (RTF list tables are out of
        // scope); `index`/`start` choose the displayed number, default 1.
        let n = match block
            .attrs
            .get("index")
            .or_else(|| block.attrs.get("start"))
        {
            Some(AttrVal::Int(n)) => *n,
            _ => 1,
        };
        let _ = write!(out, "{n}.\\tab ");
    } else {
        out.push_str("\\bullet\\tab ");
    }
    render_inline(out, &block.text, &block.marks, colors);
    out.push_str("\\par}\n");
}

fn render_image(out: &mut String, block: &BlockView, indent_level: i64) {
    // Pixel embedding is out of scope; fall back to alt text so the
    // block isn't lost, or skip if there's no alt text.
    let alt = if block.text.is_empty() {
        match block.attrs.get("alt") {
            Some(AttrVal::Str(s)) => s.clone(),
            _ => String::new(),
        }
    } else {
        block.text.clone()
    };
    if alt.is_empty() {
        return;
    }
    out.push_str("{\\pard");
    push_indent(out, indent_level);
    let _ = write!(out, "\\i\\fs{} ", BODY_PT * 2);
    push_escaped(out, &alt);
    out.push_str("\\par}\n");
}

// ---- tables -----------------------------------------------------------------

fn render_table(out: &mut String, block: &BlockView, colors: &ColorTable) {
    // Rows are child blocks, each row's children its cells. Tolerates
    // the flat shape (cells directly under the table with a `cols`
    // attr), matching qdoc-html's handling.
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

    if rows.is_empty() {
        return;
    }

    // Equal-width columns across the page width (≈ 9000 twips of usable width).
    let max_cols = rows
        .iter()
        .map(std::vec::Vec::len)
        .max()
        .unwrap_or(1)
        .max(1);
    let total_twips: i64 = 9000;
    let col_w = total_twips / max_cols as i64;

    for row in &rows {
        out.push_str("\\trowd");
        // Cell boundary positions: cumulative right edge of each column.
        for c in 1..=max_cols {
            let _ = write!(out, "\\cellx{}", col_w * c as i64);
        }
        // Cell contents.
        for cell in row {
            out.push_str("\\intbl ");
            if cell.children.is_empty() {
                render_inline(out, &cell.text, &cell.marks, colors);
            } else {
                // Block content inside a cell: flatten the first text-bearing
                // child's inline run (full nested block content is out of scope here).
                let mut tmp = String::new();
                render_blocks(&mut tmp, &cell.children, colors, 0);
                out.push_str(strip_pars(&tmp).trim());
            }
            out.push_str("\\cell ");
        }
        // Pad short rows so every row has `max_cols` cells.
        for _ in row.len()..max_cols {
            out.push_str("\\intbl \\cell ");
        }
        out.push_str("\\row\n");
    }
}

/// Remove `\pard …\par` wrappers from a rendered block buffer, leaving
/// the inline run — used to inline a cell's content into a table cell.
fn strip_pars(s: &str) -> String {
    s.replace("\\pard", "")
        .replace("\\par}", "")
        .replace(['}', '{'], "")
}

// ---- block-level formatting helpers -----------------------------------------

/// Emit a paragraph alignment control word from the block's `align` attr.
fn push_align(out: &mut String, block: &BlockView) {
    if let Some(AttrVal::Str(a)) = block.attrs.get("align") {
        let cw = match a.as_str() {
            "center" => "\\qc",
            "right" => "\\qr",
            "justify" => "\\qj",
            _ => "\\ql",
        };
        out.push_str(cw);
    }
}

/// Emit a left-indent control word for `indent_level` steps (0 = none).
fn push_indent(out: &mut String, indent_level: i64) {
    if indent_level > 0 {
        let _ = write!(out, "\\li{}", indent_level * INDENT_TWIPS);
    }
}

// ---- inline marks -----------------------------------------------------------

/// Render `text` with `marks` applied as RTF control words over the
/// correct byte ranges. Marks cut the text at byte-offset boundaries,
/// then each segment toggles the active mark set inside its own group
/// `{ … }` so toggles never leak.
fn render_inline(out: &mut String, text: &str, marks: &[MarkSpan], colors: &ColorTable) {
    if text.is_empty() {
        return;
    }
    if marks.is_empty() {
        push_escaped(out, text);
        return;
    }

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

    for w in bounds.windows(2) {
        let (seg_start, seg_end) = (w[0], w[1]);
        if seg_start >= seg_end {
            continue;
        }
        let segment = &text[seg_start..seg_end];
        let active: Vec<&MarkSpan> = marks
            .iter()
            .filter(|m| m.start <= seg_start && m.end >= seg_end && m.start < m.end)
            .collect();
        render_run(out, segment, &active, colors);
    }
}

/// Render one constant-mark-set segment. A hyperlink (`href`/`link`)
/// wraps the run in an RTF `HYPERLINK` field; other toggles share a
/// single group that resets at its close.
fn render_run(out: &mut String, segment: &str, active: &[&MarkSpan], colors: &ColorTable) {
    let href = active
        .iter()
        .find(|m| m.name == "href" || m.name == "link")
        .and_then(|m| mark_str(m));

    if let Some(url) = href {
        // {\field{\*\fldinst HYPERLINK "url"}{\fldrslt <formatted text>}}
        out.push_str("{\\field{\\*\\fldinst HYPERLINK \"");
        push_escaped(out, &url);
        out.push_str("\"}{\\fldrslt ");
        push_run_group(out, segment, active, colors);
        out.push_str("}}");
    } else {
        push_run_group(out, segment, active, colors);
    }
}

/// Emit `{ <toggles> <escaped text> }` for a run's character formatting.
fn push_run_group(out: &mut String, segment: &str, active: &[&MarkSpan], colors: &ColorTable) {
    let has = |name: &str| active.iter().any(|m| m.name == name);

    out.push('{');
    let mut wrote_cw = false;

    // Font family (`\fN`).
    if let Some(f) = active
        .iter()
        .find(|m| m.name == "font" || m.name == "fontFamily")
        .and_then(|m| mark_str(m))
    {
        let _ = write!(out, "\\f{}", font_index(&f));
        wrote_cw = true;
    }
    // Font size (`\fsN`, half-points → 2 × pt).
    if let Some(pt) = active
        .iter()
        .find(|m| m.name == "size" || m.name == "fontSize")
        .and_then(|m| m.value.as_int())
    {
        let _ = write!(out, "\\fs{}", pt * 2);
        wrote_cw = true;
    }
    // Color (`\cfN`).
    if let Some(idx) = active
        .iter()
        .find(|m| m.name == "color")
        .and_then(|m| mark_str(m))
        .as_deref()
        .and_then(parse_hex_color)
        .and_then(|rgb| colors.index_of(rgb))
    {
        let _ = write!(out, "\\cf{idx}");
        wrote_cw = true;
    }
    // Boolean toggles.
    if has("strong") || has("bold") {
        out.push_str("\\b");
        wrote_cw = true;
    }
    if has("em") || has("italic") {
        out.push_str("\\i");
        wrote_cw = true;
    }
    if has("underline") {
        out.push_str("\\ul");
        wrote_cw = true;
    }
    if has("strike") || has("strikethrough") {
        out.push_str("\\strike");
        wrote_cw = true;
    }

    // A control word must be delimited from following text by a space (the space
    // is consumed by the control word, not printed).
    if wrote_cw {
        out.push(' ');
    }
    push_escaped(out, segment);
    out.push('}');
}

/// Font-table index for a logical family name; unknown families fall
/// back to sans.
fn font_index(f: &str) -> usize {
    match f {
        "serif" => FONT_SERIF,
        "mono" | "monospace" => FONT_MONO,
        // "sans" / "sans-serif" / anything else
        _ => FONT_SANS,
    }
}

// ---- value helpers ----------------------------------------------------------

/// The string payload of a valued mark (link href, color hex, font family).
fn mark_str(m: &MarkSpan) -> Option<String> {
    m.value.as_str().map(std::string::ToString::to_string)
}

/// Parse a `#rrggbb` / `rrggbb` (or `#rgb`) hex color into `(r, g, b)`.
fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().trim_start_matches('#');
    let parse2 = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        6 => Some((parse2(0)?, parse2(2)?, parse2(4)?)),
        3 => {
            // #abc → #aabbcc
            let d = |i: usize| {
                let c = u8::from_str_radix(&h[i..=i], 16).ok()?;
                Some(c * 16 + c)
            };
            Some((d(0)?, d(1)?, d(2)?))
        }
        _ => None,
    }
}

// ---- RTF escaping -----------------------------------------------------------

/// Push `text` into `out`, escaping RTF specials and emitting non-ASCII
/// as `\uN?` unicode escapes: `\`, `{`, `}` backslash-escaped; ASCII
/// printable/whitespace verbatim (newlines become `\line `); everything
/// else as `\uN?` (signed 16-bit code unit, one per UTF-16 unit beyond the BMP).
fn push_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\n' => out.push_str("\\line "),
            '\r' => {}
            '\t' => out.push_str("\\tab "),
            c if c.is_ascii() => out.push(c),
            c => {
                // Emit each UTF-16 code unit as a signed 16-bit `\uN?`.
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let signed = *unit as i16;
                    let _ = write!(out, "\\u{signed}?");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
