//! Built-in Markdown codec. Maps the modeled rich-text subset (headings,
//! paragraphs, bullet/ordered list items, **strong** / *em* / ~~strike~~ /
//! `code`, [links](url), GitHub pipe tables, nested + task lists, and fenced
//! code blocks) onto the canonical document. Export normalizes whitespace;
//! lossless preservation of unmodeled constructs is a Phase-4 (odt) concern,
//! not here.
//!
//! Construct → model mapping:
//! - `[text](url)`         → `link` mark (value = href) over the text range.
//! - `| a | b |` tables    → consecutive `cell` blocks tagged `table`/`row`/`col`
//!                           (header row → row 0).
//! - indented sub-items    → `list_item` blocks carrying a `level` (indent) attr.
//! - `- [ ]` / `- [x]`     → `list_item` with a `checked` bool attr.
//! - `` `code` ``          → a `fontFamily`=mono mark (closest existing primitive).
//! - ```` ``` ```` fences  → a block tagged `code`=true (+ optional `lang` attr).

use crate::document::{MarkSpan, MarkValue};
use crate::format::{CodecError, Confidence, FormatProvider};
use crate::model::{DocMut, DocRef};
use crate::snapshot::{AttrVal, BlockView, DocSnapshot};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

pub struct MarkdownCodec;

impl FormatProvider for MarkdownCodec {
    fn schema_id(&self) -> &str {
        crate::schema::SCHEMA_ID
    }

    fn detect(&self, _head: &[u8]) -> Confidence {
        Confidence::Medium
    }

    fn import(&self, bytes: &[u8], doc: &mut DocMut) -> Result<(), CodecError> {
        let src = std::str::from_utf8(bytes).map_err(|e| CodecError::Parse(e.to_string()))?;
        import_md(src, doc);
        Ok(())
    }

    fn export(&self, doc: DocRef) -> Result<Vec<u8>, CodecError> {
        Ok(export_md(&doc.materialize()).into_bytes())
    }
}

/// Open a fresh top-level block, anchored after the previous one, and make it
/// the current text target.
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

/// A formatting mark gathered during parsing, applied after the full parse so
/// trailing text doesn't expand it (`add_mark` uses `ExpandMark::After`).
/// `value` is `None` for boolean toggle marks (strong/em/strike) and `Some(_)`
/// for valued marks (`link` href, `fontFamily`=mono).
struct PendingMark {
    block: String,
    start: usize,
    end: usize,
    name: &'static str,
    value: Option<String>,
}

/// A styling mark parsed from an inline `<span style=…>` `(name, value)`, e.g.
/// `("color", "#ff0000")`. The mark names match the editor's run marks so they
/// merge with marks imported from `.odt`/`.docx`.
fn marks_from_span_style(style: &str) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for decl in style.split(';') {
        let Some((prop, val)) = decl.split_once(':') else { continue };
        let prop = prop.trim().to_ascii_lowercase();
        let val = val.trim();
        match prop.as_str() {
            "color" => {
                out.push(("color", normalize_css_color(val)));
            }
            "font-size" => {
                // Accept `20pt` / `20px` / bare number; the model stores whole pt.
                let num = val.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%').trim();
                if let Ok(f) = num.parse::<f64>() {
                    out.push(("fontSize", (f.round() as i64).to_string()));
                }
            }
            "font-family" => {
                let g = generic_font_family(val);
                out.push(("fontFamily", g.to_string()));
            }
            _ => {}
        }
    }
    out
}

/// Normalise a CSS colour value to the model's canonical `#rrggbb`. Hex colours
/// lower-case; a handful of common CSS keywords map to their hex; anything else is
/// passed through trimmed (so the round-trip is at worst lossless-as-written).
fn normalize_css_color(v: &str) -> String {
    let t = v.trim();
    if let Some(hex) = t.strip_prefix('#') {
        if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return format!("#{}", hex.to_ascii_lowercase());
        }
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

/// Collapse a CSS `font-family` value onto one of the model's generic families.
fn generic_font_family(val: &str) -> &'static str {
    let v = val.to_ascii_lowercase();
    if v.contains("mono") || v.contains("courier") || v.contains("consol") {
        "mono"
    } else if v.contains("serif") && !v.contains("sans") {
        "serif"
    } else if v.contains("times") || v.contains("georgia") || v.contains("garamond") {
        "serif"
    } else {
        "sans"
    }
}

/// Extract the value of the first `style="…"` (or `style='…'`) attribute in a raw
/// HTML start tag, e.g. `<span style="color: red">` → `color: red`.
fn span_style_attr(tag: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let idx = lower.find("style")?;
    let rest = &tag[idx + "style".len()..];
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let after = &rest[1..];
    let end = after.find(quote)?;
    Some(after[..end].to_string())
}

fn import_md(src: &str, doc: &mut DocMut) {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    let mut prev: Option<String> = None; // previous top-level block (insertion anchor)
    let mut cur: Option<String> = None; // current block receiving text
    let mut cur_len: usize = 0; // char length of current block's text so far
    let mut in_item = false; // inside a list item (suppress the inner paragraph wrapper)
    let mut in_cell = false; // inside a table cell (likewise)
    let mut ordered_stack: Vec<bool> = Vec::new();
    let mut mark_stack: Vec<(&'static str, usize)> = Vec::new();
    let mut link_stack: Vec<(String, usize)> = Vec::new(); // (href, start char)
    // Open inline `<span style=…>` wrappers: (parsed (name,value) marks, start char).
    let mut span_stack: Vec<(Vec<(&'static str, String)>, usize)> = Vec::new();
    let mut pending_marks: Vec<PendingMark> = Vec::new();

    // Table state: cells become consecutive `cell` blocks tagged table/row/col.
    let mut table_id: Option<String> = None;
    let mut trow: usize = 0;
    let mut tcol: usize = 0;
    let mut table_seq: usize = 0;

    for ev in Parser::new_ext(src, opts) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "heading");
                doc.set_attr(&id, "level", level as i64);
            }
            Event::End(TagEnd::Heading(_)) => cur = None,
            Event::Start(Tag::Paragraph) => {
                if !in_item && !in_cell {
                    start_block(doc, &mut prev, &mut cur, &mut cur_len, "paragraph");
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if !in_item && !in_cell {
                    cur = None;
                }
            }
            Event::Start(Tag::List(start)) => ordered_stack.push(start.is_some()),
            Event::End(TagEnd::List(_)) => {
                ordered_stack.pop();
            }
            Event::Start(Tag::Item) => {
                let ordered = *ordered_stack.last().unwrap_or(&false);
                // Nesting depth = list stack depth - 1 (top-level lists are level 0).
                let level = ordered_stack.len().saturating_sub(1);
                let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "list_item");
                doc.set_attr(&id, "ordered", ordered);
                if level > 0 {
                    doc.set_attr(&id, "level", level as i64);
                }
                in_item = true;
            }
            Event::End(TagEnd::Item) => {
                cur = None;
                in_item = false;
            }
            Event::TaskListMarker(checked) => {
                if let Some(b) = cur.as_deref() {
                    doc.set_attr(b, "checked", checked);
                }
            }
            // ---- tables: cells map onto `cell` blocks tagged table/row/col ----
            Event::Start(Tag::Table(_)) => {
                table_seq += 1;
                table_id = Some(format!("t{table_seq}"));
                trow = 0;
            }
            Event::End(TagEnd::Table) => table_id = None,
            Event::Start(Tag::TableHead) => tcol = 0,
            Event::End(TagEnd::TableHead) => trow += 1, // header is row 0; body starts at 1
            Event::Start(Tag::TableRow) => tcol = 0,
            Event::End(TagEnd::TableRow) => trow += 1,
            Event::Start(Tag::TableCell) => {
                if let Some(tid) = table_id.clone() {
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "cell");
                    doc.set_attr(&id, "table", tid.as_str());
                    doc.set_attr(&id, "row", trow as i64);
                    doc.set_attr(&id, "col", tcol as i64);
                    in_cell = true;
                }
            }
            Event::End(TagEnd::TableCell) => {
                cur = None;
                in_cell = false;
                tcol += 1;
            }
            // ---- fenced/indented code blocks: a block tagged `code`=true ----
            Event::Start(Tag::CodeBlock(kind)) => {
                let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "paragraph");
                doc.set_attr(&id, "code", true);
                if let CodeBlockKind::Fenced(lang) = kind {
                    if !lang.is_empty() {
                        doc.set_attr(&id, "lang", lang.as_ref());
                    }
                }
            }
            Event::End(TagEnd::CodeBlock) => cur = None,
            Event::Start(Tag::Strong) => mark_stack.push(("strong", cur_len)),
            Event::End(TagEnd::Strong) => {
                if let (Some((name, start)), Some(b)) = (mark_stack.pop(), cur.as_ref()) {
                    pending_marks.push(PendingMark { block: b.clone(), start, end: cur_len, name, value: None });
                }
            }
            Event::Start(Tag::Emphasis) => mark_stack.push(("em", cur_len)),
            Event::End(TagEnd::Emphasis) => {
                if let (Some((name, start)), Some(b)) = (mark_stack.pop(), cur.as_ref()) {
                    pending_marks.push(PendingMark { block: b.clone(), start, end: cur_len, name, value: None });
                }
            }
            Event::Start(Tag::Strikethrough) => mark_stack.push(("strike", cur_len)),
            Event::End(TagEnd::Strikethrough) => {
                if let (Some((name, start)), Some(b)) = (mark_stack.pop(), cur.as_ref()) {
                    pending_marks.push(PendingMark { block: b.clone(), start, end: cur_len, name, value: None });
                }
            }
            // A hyperlink: model the href as an inline `link` mark over the
            // linked range (mirrors the .odt codec, so links round-trip across
            // formats and merge with concurrent text edits).
            Event::Start(Tag::Link { dest_url, .. }) => link_stack.push((dest_url.to_string(), cur_len)),
            Event::End(TagEnd::Link) => {
                if let (Some((href, start)), Some(b)) = (link_stack.pop(), cur.as_ref()) {
                    pending_marks.push(PendingMark { block: b.clone(), start, end: cur_len, name: "link", value: Some(href) });
                }
            }
            Event::Text(t) => {
                if let Some(b) = cur.as_ref() {
                    doc.splice_text(b, cur_len, 0, &t);
                    cur_len += t.chars().count();
                }
            }
            Event::Code(t) => {
                // Inline code → a `fontFamily`=mono mark (closest existing primitive).
                if let Some(b) = cur.as_ref() {
                    let start = cur_len;
                    doc.splice_text(b, cur_len, 0, &t);
                    cur_len += t.chars().count();
                    pending_marks.push(PendingMark {
                        block: b.clone(),
                        start,
                        end: cur_len,
                        name: "fontFamily",
                        value: Some("mono".to_string()),
                    });
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(b) = cur.as_ref() {
                    doc.splice_text(b, cur_len, 0, " ");
                    cur_len += 1;
                }
            }
            // Inline HTML carries colour/size/font marks Markdown can't express
            // natively, as `<span style=…>…</span>` (emitted by `render_inline`).
            // Each tag is its own event; track open spans on a stack.
            Event::InlineHtml(html) | Event::Html(html) => {
                let t = html.trim();
                if t.eq_ignore_ascii_case("</span>") {
                    if let (Some((marks, start)), Some(b)) = (span_stack.pop(), cur.as_ref()) {
                        for (name, value) in marks {
                            pending_marks.push(PendingMark { block: b.clone(), start, end: cur_len, name, value: Some(value) });
                        }
                    }
                } else if t.to_ascii_lowercase().starts_with("<span") {
                    let marks = span_style_attr(t).map(|s| marks_from_span_style(&s)).unwrap_or_default();
                    span_stack.push((marks, cur_len));
                }
            }
            _ => {}
        }
    }

    for pm in pending_marks {
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
}

fn export_md(snap: &DocSnapshot) -> String {
    let mut out = String::new();
    let blocks = &snap.blocks;
    let mut i = 0;
    let mut prev_kind: Option<&str> = None;
    while i < blocks.len() {
        let b = &blocks[i];
        // Separator from the previously emitted block.
        if let Some(pk) = prev_kind {
            if pk == "list_item" && b.kind == "list_item" {
                out.push('\n'); // keep consecutive list items tight
            } else {
                out.push_str("\n\n");
            }
        }

        match b.kind.as_str() {
            "cell" => {
                // Emit a run of same-`table` cells as a GitHub pipe table.
                let tid = cell_table(b);
                let start = i;
                while i < blocks.len() && blocks[i].kind == "cell" && cell_table(&blocks[i]) == tid {
                    i += 1;
                }
                out.push_str(&render_table(&blocks[start..i]));
                prev_kind = Some("cell");
                continue;
            }
            "heading" => {
                let level = match b.attrs.get("level") {
                    Some(AttrVal::Int(n)) => (*n).clamp(1, 6),
                    _ => 1,
                };
                out.push_str(&"#".repeat(level as usize));
                out.push(' ');
                out.push_str(&render_inline(&b.text, &b.marks));
            }
            "list_item" => {
                let level = match b.attrs.get("level") {
                    Some(AttrVal::Int(n)) => (*n).max(0) as usize,
                    _ => 0,
                };
                out.push_str(&"  ".repeat(level)); // 2 spaces per nesting level
                let ordered = matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true)));
                out.push_str(if ordered { "1. " } else { "- " });
                if let Some(AttrVal::Bool(checked)) = b.attrs.get("checked") {
                    out.push_str(if *checked { "[x] " } else { "[ ] " });
                }
                out.push_str(&render_inline(&b.text, &b.marks));
            }
            _ => {
                if matches!(b.attrs.get("code"), Some(AttrVal::Bool(true))) {
                    // Fenced code block: emit content verbatim (no inline escaping).
                    let lang = match b.attrs.get("lang") {
                        Some(AttrVal::Str(s)) => s.as_str(),
                        _ => "",
                    };
                    out.push_str("```");
                    out.push_str(lang);
                    out.push('\n');
                    out.push_str(&b.text);
                    if !b.text.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str("```");
                } else {
                    out.push_str(&render_inline(&b.text, &b.marks));
                }
            }
        }
        prev_kind = Some(&b.kind);
        i += 1;
    }
    out.push('\n');
    out
}

/// The `table` id a `cell` block belongs to (empty if untagged).
fn cell_table(b: &BlockView) -> String {
    match b.attrs.get("table") {
        Some(AttrVal::Str(s)) => s.clone(),
        _ => String::new(),
    }
}

/// A `cell` block's (row, col).
fn cell_rc(b: &BlockView) -> (usize, usize) {
    let g = |k: &str| match b.attrs.get(k) {
        Some(AttrVal::Int(n)) => (*n).max(0) as usize,
        _ => 0,
    };
    (g("row"), g("col"))
}

/// Escape a rendered cell so it survives one table row (pipes and newlines would
/// otherwise break the row). Re-import unescapes `\|` back to `|`.
fn escape_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Render a run of `cell` blocks (one table) as a GitHub pipe table. Row 0 is
/// the header; a delimiter row follows it.
fn render_table(cells: &[BlockView]) -> String {
    let nrows = cells.iter().map(|c| cell_rc(c).0).max().unwrap_or(0) + 1;
    let ncols = cells.iter().map(|c| cell_rc(c).1).max().unwrap_or(0) + 1;
    let mut out = String::new();
    for r in 0..nrows {
        out.push('|');
        for c in 0..ncols {
            let txt = cells
                .iter()
                .find(|cl| cell_rc(cl) == (r, c))
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

/// The Markdown delimiter for a toggle-style mark, if any. `link` is handled
/// separately (it wraps, rather than toggles). `fontFamily`=mono is the model's
/// stand-in for inline code.
fn mark_delim(m: &MarkSpan) -> Option<&'static str> {
    match m.name.as_str() {
        "strong" => Some("**"),
        "em" => Some("*"),
        "strike" => Some("~~"),
        "fontFamily" if m.value == MarkValue::Str("mono".to_string()) => Some("`"),
        _ => None,
    }
}

/// Markdown has no native syntax for text colour, font size, or (non-mono) font
/// family, so these run marks are carried as a minimal inline `<span style=…>` on
/// export and parsed back on import. This keeps `.md` lossless for the same marks
/// the `.odt`/`.docx` codecs round-trip. `fontFamily=mono` is excluded — it stays
/// the inline-code backtick form.
///
/// The CSS declaration for a styling mark covering character `i`, or `None` if the
/// mark isn't a (span-carried) styling mark.
fn span_decl(m: &MarkSpan) -> Option<String> {
    match m.name.as_str() {
        "color" => m.value.as_str().map(|c| format!("color: {c}")),
        "fontSize" => m.value.as_int().map(|n| format!("font-size: {n}pt")),
        "fontFamily" => match m.value.as_str() {
            // mono stays the backtick form (handled as inline code); sans/serif map
            // to a generic CSS family.
            Some("serif") => Some("font-family: serif".into()),
            Some("sans") => Some("font-family: sans-serif".into()),
            _ => None,
        },
        _ => None,
    }
}

/// The combined `style="…"` value for all styling marks covering character `i`,
/// or `None` if none apply. Declarations are sorted for a stable, idempotent
/// round-trip.
fn span_style_at(marks: &[MarkSpan], i: usize) -> Option<String> {
    let mut decls: Vec<String> = marks
        .iter()
        .filter(|m| i >= m.start && i < m.end)
        .filter_map(span_decl)
        .collect();
    if decls.is_empty() {
        return None;
    }
    decls.sort();
    decls.dedup();
    Some(decls.join("; "))
}

/// The active `link` href covering char index `i`, if any.
fn link_href(marks: &[MarkSpan], i: usize) -> Option<&str> {
    marks
        .iter()
        .find(|m| m.name == "link" && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_str())
}

/// Re-emit inline marks as Markdown. At each char boundary: close toggle marks
/// ending there, then handle the link transition (links wrap the toggles), then
/// open toggle marks starting there. Robust for non-overlapping marks (the
/// modeled case).
fn render_inline(text: &str, marks: &[MarkSpan]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut open_at: Vec<Vec<&'static str>> = vec![Vec::new(); n + 1];
    let mut close_at: Vec<Vec<&'static str>> = vec![Vec::new(); n + 1];
    for m in marks {
        if let Some(d) = mark_delim(m) {
            let s = m.start.min(n);
            let e = m.end.min(n);
            if s >= e {
                continue;
            }
            open_at[s].push(d);
            close_at[e].push(d);
        }
    }
    let mut out = String::new();
    let mut cur_link: Option<&str> = None;
    let mut cur_span: Option<String> = None;
    for i in 0..=n {
        for d in &close_at[i] {
            out.push_str(d);
        }
        // The span (colour/size/font) wrapper sits inside the link and around the
        // toggle marks. Close it before a link transition; reopen after.
        let span = if i < n { span_style_at(marks, i) } else { None };
        let link = if i < n { link_href(marks, i) } else { None };
        let link_changing = link != cur_link;
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
        if cur_span.is_none() {
            if let Some(style) = &span {
                out.push_str(&format!(r#"<span style="{style}">"#));
                cur_span = Some(style.clone());
            }
        }
        for d in &open_at[i] {
            out.push_str(d);
        }
        if i < n {
            out.push(chars[i]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;

    #[test]
    fn roundtrip_basic() {
        let src = "# Title\n\nHello **world** and *em*.\n\n- a\n- b\n";
        let mut d = Document::new();
        MarkdownCodec.import(src.as_bytes(), &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Title");
        assert!(snap
            .blocks
            .iter()
            .any(|b| b.kind == "paragraph" && b.marks.iter().any(|m| m.name == "strong")));
        assert_eq!(snap.blocks.iter().filter(|b| b.kind == "list_item").count(), 2);

        let out1 = MarkdownCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        MarkdownCodec.import(&out1, &mut d2.as_mut()).unwrap();
        let out2 = MarkdownCodec.export(d2.as_ref()).unwrap();
        assert_eq!(
            String::from_utf8(out1).unwrap(),
            String::from_utf8(out2).unwrap(),
            "export must be idempotent under re-import"
        );
    }

    #[test]
    fn heading_levels_preserved() {
        let mut d = Document::new();
        MarkdownCodec.import(b"## Sub", &mut d.as_mut()).unwrap();
        let out = String::from_utf8(MarkdownCodec.export(d.as_ref()).unwrap()).unwrap();
        assert!(out.starts_with("## Sub"), "got {out:?}");
    }

    #[test]
    fn ordered_list_marker() {
        let mut d = Document::new();
        MarkdownCodec.import(b"1. one\n2. two\n", &mut d.as_mut()).unwrap();
        let out = String::from_utf8(MarkdownCodec.export(d.as_ref()).unwrap()).unwrap();
        assert!(out.contains("1. one"), "got {out:?}");
    }

    fn import(src: &str) -> DocSnapshot {
        let mut d = Document::new();
        MarkdownCodec.import(src.as_bytes(), &mut d.as_mut()).unwrap();
        d.materialize()
    }

    fn export(snap_src: &str) -> String {
        let mut d = Document::new();
        MarkdownCodec.import(snap_src.as_bytes(), &mut d.as_mut()).unwrap();
        String::from_utf8(MarkdownCodec.export(d.as_ref()).unwrap()).unwrap()
    }

    #[test]
    fn link_roundtrips_as_mark() {
        let src = "See [the docs](https://qos.dev/) now.\n";
        let snap = import(src);
        let p = &snap.blocks[0];
        assert_eq!(p.kind, "paragraph");
        assert_eq!(p.text, "See the docs now.");
        let link = p.marks.iter().find(|m| m.name == "link").expect("link mark present");
        assert_eq!(link.value, MarkValue::Str("https://qos.dev/".into()));
        // The mark covers exactly "the docs".
        assert_eq!((link.start, link.end), (4, 12));

        let out = export(src);
        assert!(out.contains("[the docs](https://qos.dev/)"), "got {out:?}");
        // Round-trip is idempotent.
        assert_eq!(export(&out), out);
    }

    #[test]
    fn pipe_table_roundtrips_as_cells() {
        let src = "| a | b |\n| --- | --- |\n| 1 | 2 |\n";
        let snap = import(src);
        let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.len(), 4, "2x2 cells: {snap:?}");
        assert_eq!(cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(), vec!["a", "b", "1", "2"]);
        // Header row is row 0; body row is row 1. All share one table id.
        let tid = match cells[0].attrs.get("table") {
            Some(AttrVal::Str(s)) => s.clone(),
            other => panic!("cell must carry a table id, got {other:?}"),
        };
        assert!(cells.iter().all(|c| matches!(c.attrs.get("table"), Some(AttrVal::Str(s)) if *s == tid)));
        assert_eq!(cells[0].attrs.get("row"), Some(&AttrVal::Int(0)));
        assert_eq!(cells[0].attrs.get("col"), Some(&AttrVal::Int(0)));
        assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
        assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));

        let out = export(src);
        assert!(out.contains("| a | b |"), "header row missing: {out:?}");
        assert!(out.contains("| --- | --- |"), "delimiter row missing: {out:?}");
        assert!(out.contains("| 1 | 2 |"), "body row missing: {out:?}");
        assert_eq!(export(&out), out, "table export idempotent");
    }

    #[test]
    fn nested_lists_carry_level_and_reindent() {
        let src = "- top\n  - sub\n  - sub2\n- top2\n";
        let snap = import(src);
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").collect();
        assert_eq!(items.len(), 4);
        let level = |b: &BlockView| match b.attrs.get("level") {
            Some(AttrVal::Int(n)) => *n,
            _ => 0,
        };
        assert_eq!(items.iter().map(|b| (b.text.clone(), level(b))).collect::<Vec<_>>(), vec![
            ("top".to_string(), 0),
            ("sub".to_string(), 1),
            ("sub2".to_string(), 1),
            ("top2".to_string(), 0),
        ]);

        let out = export(src);
        assert!(out.contains("- top\n  - sub\n  - sub2\n- top2"), "re-indent failed: {out:?}");
        assert_eq!(export(&out), out, "nested-list export idempotent");
    }

    #[test]
    fn task_lists_roundtrip_checked_attr() {
        let src = "- [ ] todo\n- [x] done\n";
        let snap = import(src);
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].attrs.get("checked"), Some(&AttrVal::Bool(false)));
        assert_eq!(items[1].attrs.get("checked"), Some(&AttrVal::Bool(true)));

        let out = export(src);
        assert!(out.contains("- [ ] todo"), "unchecked box lost: {out:?}");
        assert!(out.contains("- [x] done"), "checked box lost: {out:?}");
        assert_eq!(export(&out), out, "task-list export idempotent");
    }

    #[test]
    fn inline_code_roundtrips_as_mono_mark() {
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
        assert_eq!((mono.start, mono.end), (4, 14)); // "cargo test"

        let out = export(src);
        assert!(out.contains("`cargo test`"), "got {out:?}");
        assert_eq!(export(&out), out, "inline-code export idempotent");
    }

    #[test]
    fn fenced_code_block_preserves_content() {
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
        assert!(out.contains("```rust\nlet x = 1;\nlet y = 2;\n```"), "code block lost: {out:?}");
        assert_eq!(export(&out), out, "fenced-code export idempotent");
    }

    #[test]
    fn span_carries_color_size_font_through_markdown() {
        // Markdown has no native colour/size/font syntax, so these run marks ride
        // as an inline `<span style=…>` on export and parse back on import.
        let mut d = Document::new();
        let id = d.insert_block(None, "paragraph");
        d.splice_text(&id, 0, 0, "styled word");
        {
            let mut dm = d.as_mut();
            dm.add_mark(&id, 0, 6, "color", "#ff0000");
            dm.add_mark(&id, 0, 6, "fontSize", 20_i64);
            dm.add_mark(&id, 0, 6, "fontFamily", "serif");
        }
        let out = String::from_utf8(MarkdownCodec.export(d.as_ref()).unwrap()).unwrap();
        assert!(out.contains("<span style="), "emits an inline span: {out:?}");
        assert!(out.contains("color: #ff0000"), "colour decl: {out:?}");
        assert!(out.contains("font-size: 20pt"), "size decl: {out:?}");
        assert!(out.contains("font-family: serif"), "font decl: {out:?}");

        let mut d2 = Document::new();
        MarkdownCodec.import(out.as_bytes(), &mut d2.as_mut()).unwrap();
        let b = &d2.materialize().blocks[0];
        assert_eq!(b.text, "styled word");
        assert!(
            b.marks.iter().any(|m| m.name == "color" && m.value == MarkValue::Str("#ff0000".into()) && m.start == 0 && m.end == 6),
            "colour mark survives: {:?}",
            b.marks
        );
        assert!(
            b.marks.iter().any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20) && m.start == 0 && m.end == 6),
            "size mark survives as Int: {:?}",
            b.marks
        );
        assert!(
            b.marks.iter().any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into()) && m.start == 0 && m.end == 6),
            "font mark survives: {:?}",
            b.marks
        );

        // Idempotent under re-export.
        assert_eq!(export(&out), out, "span export idempotent");
    }

    #[test]
    fn strikethrough_roundtrips() {
        let src = "this is ~~gone~~ ok\n";
        let snap = import(src);
        let p = &snap.blocks[0];
        assert_eq!(p.text, "this is gone ok");
        assert!(p.marks.iter().any(|m| m.name == "strike"), "strike mark present: {:?}", p.marks);
        let out = export(src);
        assert!(out.contains("~~gone~~"), "got {out:?}");
        assert_eq!(export(&out), out);
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
            MarkdownCodec.import(src.as_bytes(), &mut d.as_mut()).unwrap();
            let out1 = MarkdownCodec.export(d.as_ref()).unwrap();
            let mut d2 = Document::new();
            MarkdownCodec.import(&out1, &mut d2.as_mut()).unwrap();
            let out2 = MarkdownCodec.export(d2.as_ref()).unwrap();
            prop_assert_eq!(String::from_utf8(out1).unwrap(), String::from_utf8(out2).unwrap());
        }
    }
}
