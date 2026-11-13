use std::collections::BTreeMap;
use std::fmt::Write as _;
use tilo_doc::export::{BlockKind, ExportBlock, TablePlan, walk_blocks};
use tilo_doc::snapshot::AttrVal;
use tilo_doc::{BlockView, DocSnapshot, InlinePlan, MarkSpan};

/// Font-table indices. RTF references fonts by `\fN`; these are the
/// slots registered in `\fonttbl` in this fixed order.
const FONT_SANS: usize = 0;
const FONT_SERIF: usize = 1;
const FONT_MONO: usize = 2;

const BODY_PT: i64 = 11;

/// Twips per indent step (RTF measures indents in twips; 720 ≈ 0.5 inch).
const INDENT_TWIPS: i64 = 720;

/// Render a materialized qdoc snapshot to an RTF string. Always
/// succeeds: unknown block kinds degrade to a paragraph, image blocks
/// degrade to their alt text (or are skipped).
pub fn render_rtf(snap: &DocSnapshot) -> String {
    let snap = &*tilo_doc::export::without_kept_placeholders(snap);
    let mut colors = ColorTable::new();
    collect_colors(&snap.blocks, &mut colors);

    // Pass 2: render the body into a buffer (never references colors not
    // already collected, since collection walked the same blocks).
    let mut body = String::new();
    render_blocks(&mut body, &snap.blocks, &colors, 0);

    let mut out = String::new();
    out.push_str("{\\rtf1\\ansi\\deff0");
    out.push_str(FONT_TABLE);
    out.push_str(&colors.render());
    out.push('\n');
    out.push_str(&body);
    out.push('}');
    out
}

const FONT_TABLE: &str = "{\\fonttbl\
{\\f0\\fswiss Helvetica;}\
{\\f1\\froman Times;}\
{\\f2\\fmodern Courier;}}";

/// Collects distinct RGB colors and renders the RTF `\colortbl`. RTF
/// reserves color index 0 as "auto", so registered colors start at 1;
/// `index_of` returns the 1-based `\cfN` index.
struct ColorTable {
    /// Insertion-ordered distinct colors as `(r, g, b)`.
    colors: Vec<(u8, u8, u8)>,
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
            return pos.saturating_add(1);
        }
        let pos = self.colors.len();
        self.colors.push(rgb);
        self.seen.insert(rgb, pos);
        pos.saturating_add(1)
    }

    /// The 1-based RTF color index for an already-registered color, if present.
    fn index_of(&self, rgb: (u8, u8, u8)) -> Option<usize> {
        self.seen.get(&rgb).map(|&p| p.saturating_add(1))
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

fn collect_colors(blocks: &[BlockView], colors: &mut ColorTable) {
    for b in walk_blocks(blocks) {
        for m in &b.marks {
            if m.name == "color"
                && let Some(hex) = mark_str(m).as_deref().and_then(parse_hex_color)
            {
                colors.intern(hex);
            }
        }
    }
}

fn render_blocks(out: &mut String, blocks: &[BlockView], colors: &ColorTable, indent_level: i64) {
    for block in blocks {
        render_block(out, block, colors, indent_level);
    }
}

fn render_block(out: &mut String, block: &BlockView, colors: &ColorTable, indent_level: i64) {
    match ExportBlock::new(block).kind() {
        BlockKind::Heading => render_heading(out, block, colors),
        BlockKind::Quote => render_quote(out, block, colors, indent_level),
        BlockKind::ListItem => render_list_item(out, block, colors),
        BlockKind::Table => render_table(out, block, colors),
        BlockKind::Code => render_code(out, block),
        BlockKind::Image => render_image(out, block, indent_level),
        BlockKind::Paragraph => render_paragraph(out, block, colors, indent_level),
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
    let level = i64::from(ExportBlock::new(block).heading_level());
    let pt = heading_pt(level);
    out.push_str("{\\pard");
    push_align(out, block);
    let _ = write!(out, "\\b\\fs{}", pt.saturating_mul(2));
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
    out.push_str("{\\pard");
    let child_indent = indent_level.saturating_add(1);
    push_indent(out, child_indent);
    push_align(out, block);
    let _ = write!(out, "\\i\\fs{}", BODY_PT * 2);
    out.push(' ');
    if !block.text.is_empty() {
        render_inline(out, &block.text, &block.marks, colors);
    }
    out.push_str("\\par}\n");
    render_blocks(out, &block.children, colors, child_indent);
}

fn render_code(out: &mut String, block: &BlockView) {
    let _ = write!(out, "{{\\pard\\f{FONT_MONO}\\fs{} ", BODY_PT * 2);
    push_escaped(out, &block.text);
    out.push_str("\\par}\n");
}

fn render_list_item(out: &mut String, block: &BlockView, colors: &ColorTable) {
    let level = ExportBlock::new(block).list_level();
    let ordered = ExportBlock::new(block).ordered();
    out.push_str("{\\pard");
    push_indent(out, level.saturating_add(1));
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
    let alt = if block.text.is_empty() {
        block.str_attr("alt").unwrap_or_default().to_owned()
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

fn render_table(out: &mut String, block: &BlockView, colors: &ColorTable) {
    let plan = TablePlan::new(block);

    if plan.is_empty() {
        return;
    }

    let max_cols = plan
        .rows()
        .iter()
        .map(std::vec::Vec::len)
        .max()
        .unwrap_or(1)
        .max(1);
    let total_twips: i64 = 9000;
    let max_cols_i64 = i64::try_from(max_cols).unwrap_or(i64::MAX).max(1);
    let col_w = total_twips.checked_div(max_cols_i64).unwrap_or(total_twips);

    for row in plan.rows() {
        out.push_str("\\trowd");
        for c in 1..=max_cols {
            let c_i64 = i64::try_from(c).unwrap_or(i64::MAX);
            let _ = write!(out, "\\cellx{}", col_w.saturating_mul(c_i64));
        }
        for cell in row {
            out.push_str("\\intbl ");
            if cell.children.is_empty() {
                render_inline(out, &cell.text, &cell.marks, colors);
            } else {
                let mut tmp = String::new();
                render_blocks(&mut tmp, &cell.children, colors, 0);
                out.push_str(strip_pars(&tmp).trim());
            }
            out.push_str("\\cell ");
        }
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
        let _ = write!(out, "\\li{}", indent_level.saturating_mul(INDENT_TWIPS));
    }
}

/// Render `text` with qdoc's canonical character-offset mark traversal.
fn render_inline(out: &mut String, text: &str, marks: &[MarkSpan], colors: &ColorTable) {
    if text.is_empty() {
        return;
    }
    if marks.is_empty() {
        push_escaped(out, text);
        return;
    }

    for point in InlinePlan::new(text, marks).points() {
        let Some(run) = &point.run else {
            continue;
        };
        render_run(out, run.text, &run.marks, colors);
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
        out.push_str("{\\field{\\*\\fldinst HYPERLINK \"");
        push_escaped(out, &url);
        out.push_str("\"}{\\fldrslt ");
        push_run_group(out, segment, active, colors);
        out.push_str("}}");
    } else {
        push_run_group(out, segment, active, colors);
    }
}

fn push_run_group(out: &mut String, segment: &str, active: &[&MarkSpan], colors: &ColorTable) {
    let has = |name: &str| active.iter().any(|m| m.name == name);

    out.push('{');
    let words_start = out.len();
    if let Some(f) = active
        .iter()
        .find(|m| m.name == "font" || m.name == "fontFamily")
        .and_then(|m| mark_str(m))
    {
        let _ = write!(out, "\\f{}", font_index(&f));
    }
    if let Some(pt) = active
        .iter()
        .find(|m| m.name == "size" || m.name == "fontSize")
        .and_then(|m| m.value.as_int())
    {
        let _ = write!(out, "\\fs{}", pt.saturating_mul(2));
    }
    if let Some(idx) = active
        .iter()
        .find(|m| m.name == "color")
        .and_then(|m| mark_str(m))
        .as_deref()
        .and_then(parse_hex_color)
        .and_then(|rgb| colors.index_of(rgb))
    {
        let _ = write!(out, "\\cf{idx}");
    }
    if has("strong") || has("bold") {
        out.push_str("\\b");
    }
    if has("em") || has("italic") {
        out.push_str("\\i");
    }
    if has("underline") {
        out.push_str("\\ul");
    }
    if has("strike") || has("strikethrough") {
        out.push_str("\\strike");
    }

    // A control word must be delimited from following text by a space (the space
    // is consumed by the control word, not printed).
    if out.len() > words_start {
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
        _ => FONT_SANS,
    }
}

fn mark_str(m: &MarkSpan) -> Option<String> {
    m.value.as_str().map(std::string::ToString::to_string)
}

fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().trim_start_matches('#');
    let parse2 = |i: usize| {
        h.get(i..i.saturating_add(2))
            .and_then(|s| u8::from_str_radix(s, 16).ok())
    };
    match h.len() {
        6 => Some((parse2(0)?, parse2(2)?, parse2(4)?)),
        3 => {
            let d = |i: usize| {
                let c = u8::from_str_radix(h.get(i..=i)?, 16).ok()?;
                c.checked_mul(16)?.checked_add(c)
            };
            Some((d(0)?, d(1)?, d(2)?))
        }
        _ => None,
    }
}

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
                    // RTF's `\uN` control word wants the code unit's bits
                    // reinterpreted as signed 16-bit — this is the intended
                    // encoding, not a lossy truncation.
                    let signed = unit.cast_signed();
                    let _ = write!(out, "\\u{signed}?");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
