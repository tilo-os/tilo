//! Built-in RTF (Rich Text Format) **import** codec. The inverse of the
//! `docrtf` export crate: it parses an `{\rtf1 …}` document back onto the
//! canonical rich-text model so the word processor can *open* `.rtf` files.
//!
//! RTF is a plain-text control-word format. The grammar this parser understands:
//!
//! - **groups** `{ … }` — push/pop the formatting state (a balanced brace stack).
//! - **control words** `\word` / `\wordN` — a keyword plus an optional signed
//!   numeric parameter, terminated by a space (consumed) or any non-alnum char.
//! - **control symbols** `\\` `\{` `\}` (literal specials), `\'XX` (a hex byte),
//!   `\uN` (a signed-16-bit Unicode code unit followed by a fallback char), and
//!   `\*` (skip-this-destination marker).
//! - **plain text** — everything else, spliced into the current block.
//!
//! ### Control-word → model mapping (mirrors `docrtf`'s emit side)
//!
//! - `\par` / `\pard` → close the current paragraph; the next text opens a fresh
//!   `paragraph` block. `\pard` also resets paragraph-level state (indent, list).
//! - `\b` / `\b0`, `\i` / `\i0`, `\ul` / `\ulnone` (or `\ul0`), `\strike` /
//!   `\strike0` → `strong` / `em` / `underline` / `strike` boolean run marks.
//! - `\cfN` → a `color` mark valued with the `#rrggbb` of color-table entry `N`
//!   (index 0 is the reserved "auto" entry and clears the color).
//! - `\fsN` → a `fontSize` mark in **points** (`N` is half-points, so `N / 2`).
//! - `\bullet` / `\b7` list bullet glyphs, and `\pntext` list-marker groups, are
//!   best-effort: a paragraph carrying a bullet becomes a `list_item` block.
//! - `\fonttbl`, `\stylesheet`, `\info`, `\*\…` and other non-content
//!   destinations are skipped wholesale (their text is not document body).
//! - `\colortbl` is parsed (not skipped) to build the color lookup.
//! - Escapes `\\ \{ \}`, `\'XX` hex bytes, and `\uN` Unicode → literal text.
//!
//! RTF has **no semantic headings**, so every body paragraph maps to a
//! `paragraph` block (a bulleted one to `list_item`) — heading reconstruction
//! would require style-table heuristics that are out of scope for a best-effort
//! importer. `export` is intentionally `Unsupported`: RTF *export* lives in the
//! editor via the `docrtf` crate (it renders from a materialized snapshot, which
//! a `FormatProvider` does not get), so there is exactly one RTF writer.

use crate::format::{CodecError, Confidence, FormatProvider};
use crate::model::{DocMut, DocRef};

pub struct RtfCodec;

impl FormatProvider for RtfCodec {
    fn schema_id(&self) -> &str {
        crate::schema::SCHEMA_ID
    }

    fn detect(&self, head: &[u8]) -> Confidence {
        // The RTF signature is a leading `{\rtf` (optionally after a BOM/whitespace).
        let s = head.iter().take_while(|b| b.is_ascii()).cloned().collect::<Vec<u8>>();
        let trimmed = s.iter().skip_while(|b| b.is_ascii_whitespace()).cloned().collect::<Vec<u8>>();
        if trimmed.starts_with(b"{\\rtf") {
            Confidence::High
        } else {
            Confidence::None
        }
    }

    fn import(&self, bytes: &[u8], doc: &mut DocMut) -> Result<(), CodecError> {
        // RTF is ASCII-with-escapes; non-ASCII bytes only appear inside `\'XX`
        // hex escapes or `\uN` units, both of which we decode ourselves. Treat the
        // input as Latin-1-ish: map each byte to a char so the tokenizer indexes
        // by char == byte for the control-word/brace machinery, and raw high bytes
        // (rare; most encoders escape them) degrade to their code point.
        let src: String = bytes.iter().map(|&b| b as char).collect();
        import_rtf(&src, doc);
        Ok(())
    }

    fn export(&self, _doc: DocRef) -> Result<Vec<u8>, CodecError> {
        // Export deliberately lives in the editor via `docrtf::render_rtf`, which
        // renders from a materialized `DocSnapshot`. A `FormatProvider::export`
        // only gets a `DocRef`; rather than duplicate the writer here we keep a
        // single source of truth and surface a clear error if someone routes RTF
        // export through the registry.
        Err(CodecError::Unsupported(
            "RTF export is provided by the `docrtf` crate (editor), not the qdoc codec".to_string(),
        ))
    }
}

// ---- tokenizer --------------------------------------------------------------

/// One lexical unit of an RTF stream.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    GroupOpen,
    GroupClose,
    /// A control word `\word` with an optional signed numeric parameter.
    Control(String, Option<i32>),
    /// A control symbol: a single non-alpha char after `\` (`\\`, `\{`, `\}`,
    /// `\*`, `\~`, `\-`, etc.). `\'XX` and `\uN` are lexed as their own tokens.
    Symbol(char),
    /// A `\'XX` hex byte escape → the decoded byte value.
    HexByte(u8),
    /// A `\uN` Unicode escape → the signed 16-bit code unit `N`.
    Unicode(i32),
    /// A run of literal text.
    Text(String),
}

/// Tokenize the RTF source. Operates over chars (== bytes after the import-side
/// byte→char map), so indexing is straightforward.
fn tokenize(src: &str) -> Vec<Token> {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut out = Vec::new();
    let mut text = String::new();

    macro_rules! flush_text {
        () => {
            if !text.is_empty() {
                out.push(Token::Text(std::mem::take(&mut text)));
            }
        };
    }

    while i < n {
        let c = chars[i];
        match c {
            '{' => {
                flush_text!();
                out.push(Token::GroupOpen);
                i += 1;
            }
            '}' => {
                flush_text!();
                out.push(Token::GroupClose);
                i += 1;
            }
            // CR/LF inside the RTF stream are insignificant (not document text).
            '\r' | '\n' => {
                i += 1;
            }
            '\\' => {
                flush_text!();
                i += 1;
                if i >= n {
                    break;
                }
                let d = chars[i];
                if d.is_ascii_alphabetic() {
                    // Control word: letters, then an optional signed integer.
                    let mut word = String::new();
                    while i < n && chars[i].is_ascii_alphabetic() {
                        word.push(chars[i]);
                        i += 1;
                    }
                    let mut param: Option<i32> = None;
                    if i < n && (chars[i] == '-' || chars[i].is_ascii_digit()) {
                        let mut num = String::new();
                        if chars[i] == '-' {
                            num.push('-');
                            i += 1;
                        }
                        while i < n && chars[i].is_ascii_digit() {
                            num.push(chars[i]);
                            i += 1;
                        }
                        param = num.parse::<i32>().ok();
                    }
                    // A single trailing space is the control-word delimiter and is
                    // consumed (not literal text).
                    if i < n && chars[i] == ' ' {
                        i += 1;
                    }
                    // `\u` Unicode escape: the parameter is the code unit; an RTF
                    // `\ucN` count governs how many following fallback chars to
                    // skip (default 1). We skip exactly one byte/char of fallback.
                    if word == "u" {
                        if let Some(cu) = param {
                            out.push(Token::Unicode(cu));
                            // Skip one fallback char (the `?` `docrtf` emits, or a
                            // literal substitute). A control word counts as one.
                            i = skip_one_fallback(&chars, i, n);
                        }
                    } else {
                        out.push(Token::Control(word, param));
                    }
                } else if d == '\'' {
                    // `\'XX` — two hex digits → one byte.
                    i += 1;
                    let mut hex = String::new();
                    while i < n && hex.len() < 2 && chars[i].is_ascii_hexdigit() {
                        hex.push(chars[i]);
                        i += 1;
                    }
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        out.push(Token::HexByte(byte));
                    }
                } else {
                    // Control symbol: the single char is the symbol. `\\ \{ \}`
                    // are literal specials; others (`\*`, `\~`, `\-`, …) carry
                    // meaning the parser interprets.
                    out.push(Token::Symbol(d));
                    i += 1;
                }
            }
            _ => {
                text.push(c);
                i += 1;
            }
        }
    }
    flush_text!();
    out
}

/// Skip exactly one Unicode-fallback unit after a `\uN` escape: either a single
/// literal char, or a whole `\'XX` / control-word substitute. Returns the new
/// index. (RTF's `\ucN` defaults to 1, which is what `docrtf` and most encoders
/// emit; we don't track a custom `\uc`.)
fn skip_one_fallback(chars: &[char], mut i: usize, n: usize) -> usize {
    if i >= n {
        return i;
    }
    if chars[i] == '\\' {
        // A `\'XX` or `\word` fallback counts as one unit.
        i += 1;
        if i < n && chars[i] == '\'' {
            i += 1;
            let mut k = 0;
            while i < n && k < 2 && chars[i].is_ascii_hexdigit() {
                i += 1;
                k += 1;
            }
        } else {
            while i < n && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
        }
    } else {
        i += 1; // a single literal char
    }
    i
}

// ---- color table ------------------------------------------------------------

/// Parse a `{\colortbl;\redR\greenG\blueB; …}` group's tokens (after the leading
/// `\colortbl`) into a 1-based list of `#rrggbb` strings. Index 0 (the leading
/// empty `;`) is the reserved "auto" entry and maps to `None`.
fn parse_color_table(tokens: &[Token]) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = Vec::new();
    let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
    let mut saw_component = false;
    for t in tokens {
        match t {
            Token::Control(w, p) => {
                let v = p.unwrap_or(0).clamp(0, 255) as u32;
                match w.as_str() {
                    "red" => { r = v; saw_component = true; }
                    "green" => { g = v; saw_component = true; }
                    "blue" => { b = v; saw_component = true; }
                    _ => {}
                }
            }
            // A `;` terminates one color-table entry.
            Token::Text(s) => {
                for ch in s.chars() {
                    if ch == ';' {
                        if saw_component {
                            out.push(Some(format!("#{r:02x}{g:02x}{b:02x}")));
                        } else {
                            out.push(None); // the reserved/auto slot
                        }
                        r = 0;
                        g = 0;
                        b = 0;
                        saw_component = false;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// ---- import state machine ---------------------------------------------------

/// Per-scope character formatting, pushed/popped on `{`/`}`.
#[derive(Clone, Default)]
struct CharState {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    /// 1-based color-table index (`\cfN`); 0 = auto/none.
    color: usize,
    /// Font size in half-points (`\fsN`); `None` = unset (no `size` mark).
    fs_halfpt: Option<i32>,
}

/// A run mark gathered during parsing, applied after the block's text is
/// complete (so `ExpandMark::After` doesn't stretch it over trailing text).
struct PendingMark {
    block: String,
    start: usize,
    end: usize,
    name: &'static str,
    value: Option<MarkVal>,
}

enum MarkVal {
    Str(String),
    Int(i64),
}

fn import_rtf(src: &str, doc: &mut DocMut) {
    let tokens = tokenize(src);

    // Brace-depth scoped char-format stack. Index 0 is the document scope.
    let mut stack: Vec<CharState> = vec![CharState::default()];
    let mut colors: Vec<Option<String>> = Vec::new();

    // Current block being filled, its char length, and whether the *next* text
    // should open a fresh paragraph (true initially and after every `\par`).
    let mut prev_block: Option<String> = None;
    let mut cur_block: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut need_new_block = true;
    // Whether the pending/just-started paragraph is a list item (saw a bullet).
    let mut pending_bullet = false;

    let mut pending_marks: Vec<PendingMark> = Vec::new();

    // Skip-destination depth: when we enter an ignorable destination (`\*`, or a
    // known non-content one like `\fonttbl`/`\info`/`\stylesheet`), we drop all
    // tokens until its closing brace. Tracked as the brace depth at which the
    // skip began (`None` = not skipping).
    let mut skip_depth: Option<usize> = None;
    let mut depth: usize = 0;

    let mut idx = 0;
    while idx < tokens.len() {
        let tok = &tokens[idx];

        // ---- color table: intercept the whole `\colortbl … ;}` group --------
        if skip_depth.is_none() {
            if let Token::Control(w, _) = tok {
                if w == "colortbl" {
                    // Consume tokens until this group's closing brace.
                    let mut j = idx + 1;
                    let mut inner_depth = 1; // we are inside the colortbl's group
                    let start = j;
                    while j < tokens.len() && inner_depth > 0 {
                        match &tokens[j] {
                            Token::GroupOpen => inner_depth += 1,
                            Token::GroupClose => inner_depth -= 1,
                            _ => {}
                        }
                        if inner_depth == 0 {
                            break;
                        }
                        j += 1;
                    }
                    colors = parse_color_table(&tokens[start..j]);
                    // Leave the closing brace for the normal handler to pop depth.
                    idx = j;
                    continue;
                }
            }
        }

        match tok {
            Token::GroupOpen => {
                depth += 1;
                let top = stack.last().cloned().unwrap_or_default();
                stack.push(top);
            }
            Token::GroupClose => {
                if stack.len() > 1 {
                    stack.pop();
                }
                if let Some(sd) = skip_depth {
                    if depth == sd {
                        skip_depth = None;
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Token::Symbol(sym) => {
                if skip_depth.is_some() {
                    idx += 1;
                    continue;
                }
                match sym {
                    // `\*` marks the *current* group as an ignorable destination.
                    '*' => skip_depth = Some(depth),
                    // Literal specials.
                    '\\' => push_char(doc, &mut prev_block, &mut cur_block, &mut cur_len, &mut need_new_block, &mut pending_bullet, '\\'),
                    '{' => push_char(doc, &mut prev_block, &mut cur_block, &mut cur_len, &mut need_new_block, &mut pending_bullet, '{'),
                    '}' => push_char(doc, &mut prev_block, &mut cur_block, &mut cur_len, &mut need_new_block, &mut pending_bullet, '}'),
                    // Non-breaking space / optional hyphen → their literal glyphs.
                    '~' => push_char(doc, &mut prev_block, &mut cur_block, &mut cur_len, &mut need_new_block, &mut pending_bullet, '\u{a0}'),
                    '_' => push_char(doc, &mut prev_block, &mut cur_block, &mut cur_len, &mut need_new_block, &mut pending_bullet, '\u{2011}'),
                    '-' => {} // optional hyphen: no visible glyph
                    _ => {}
                }
            }
            Token::Control(word, param) => {
                if skip_depth.is_some() {
                    idx += 1;
                    continue;
                }
                handle_control(
                    word,
                    *param,
                    &mut stack,
                    &mut skip_depth,
                    depth,
                    doc,
                    &mut prev_block,
                    &mut cur_block,
                    &mut cur_len,
                    &mut need_new_block,
                    &mut pending_bullet,
                    &colors,
                    &mut pending_marks,
                );
            }
            Token::HexByte(b) => {
                if skip_depth.is_some() {
                    idx += 1;
                    continue;
                }
                // Decode the byte as Latin-1 (the common ANSI fallback). Most
                // documents using a different code page still escape ASCII as
                // ASCII, so this is correct for the printable range and a
                // reasonable best-effort above it.
                push_char(doc, &mut prev_block, &mut cur_block, &mut cur_len, &mut need_new_block, &mut pending_bullet, *b as char);
            }
            Token::Unicode(cu) => {
                if skip_depth.is_some() {
                    idx += 1;
                    continue;
                }
                // A signed 16-bit code unit. Reinterpret negatives as their
                // unsigned u16, then decode (lone surrogates degrade to U+FFFD).
                let u = (*cu as i64 & 0xffff) as u16;
                let ch = char::from_u32(u as u32).unwrap_or('\u{fffd}');
                push_char(doc, &mut prev_block, &mut cur_block, &mut cur_len, &mut need_new_block, &mut pending_bullet, ch);
            }
            Token::Text(s) => {
                if skip_depth.is_some() {
                    idx += 1;
                    continue;
                }
                push_text(
                    s,
                    stack.last().cloned().unwrap_or_default(),
                    doc,
                    &mut prev_block,
                    &mut cur_block,
                    &mut cur_len,
                    &mut need_new_block,
                    &mut pending_bullet,
                    &colors,
                    &mut pending_marks,
                );
            }
        }
        idx += 1;
    }

    // Apply gathered run marks now that all text is in place.
    for pm in pending_marks {
        if pm.end <= pm.start {
            continue;
        }
        match pm.value {
            Some(MarkVal::Int(n)) => doc.add_mark(&pm.block, pm.start, pm.end, pm.name, n),
            Some(MarkVal::Str(s)) => doc.add_mark(&pm.block, pm.start, pm.end, pm.name, s.as_str()),
            None => doc.add_mark(&pm.block, pm.start, pm.end, pm.name, true),
        }
    }
}

/// Resolve `\cfN` to a `#rrggbb` string, or `None` (auto / out of range).
fn color_hex(colors: &[Option<String>], idx: usize) -> Option<String> {
    if idx == 0 {
        return None;
    }
    colors.get(idx).cloned().flatten()
}

/// Open a fresh `paragraph` (or `list_item`) block and make it current.
fn open_block(
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    is_list: bool,
) {
    let kind = if is_list { "list_item" } else { "paragraph" };
    let id = doc.insert_block(prev.as_deref(), kind);
    if is_list {
        doc.set_attr(&id, "ordered", false);
    }
    *prev = Some(id.clone());
    *cur = Some(id);
    *cur_len = 0;
}

/// Ensure a current block exists (opening one if a paragraph break is pending),
/// then return its id.
#[allow(clippy::too_many_arguments)]
fn ensure_block(
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    need_new_block: &mut bool,
    pending_bullet: &mut bool,
) -> String {
    if *need_new_block || cur.is_none() {
        open_block(doc, prev, cur, cur_len, *pending_bullet);
        *need_new_block = false;
        *pending_bullet = false;
    }
    cur.clone().expect("block was just opened")
}

/// Push a single literal char into the current block.
#[allow(clippy::too_many_arguments)]
fn push_char(
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    need_new_block: &mut bool,
    pending_bullet: &mut bool,
    ch: char,
) {
    let id = ensure_block(doc, prev, cur, cur_len, need_new_block, pending_bullet);
    let mut buf = [0u8; 4];
    doc.splice_text(&id, *cur_len, 0, ch.encode_utf8(&mut buf));
    *cur_len += 1;
}

/// Push a run of literal text under the active char formatting, gathering the
/// run marks it implies.
#[allow(clippy::too_many_arguments)]
fn push_text(
    s: &str,
    cs: CharState,
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    need_new_block: &mut bool,
    pending_bullet: &mut bool,
    colors: &[Option<String>],
    pending: &mut Vec<PendingMark>,
) {
    if s.is_empty() {
        return;
    }
    let id = ensure_block(doc, prev, cur, cur_len, need_new_block, pending_bullet);
    let start = *cur_len;
    doc.splice_text(&id, start, 0, s);
    let len = s.chars().count();
    let end = start + len;
    *cur_len = end;

    // Gather the marks the active char state implies over [start, end).
    if cs.bold {
        pending.push(PendingMark { block: id.clone(), start, end, name: "strong", value: None });
    }
    if cs.italic {
        pending.push(PendingMark { block: id.clone(), start, end, name: "em", value: None });
    }
    if cs.underline {
        pending.push(PendingMark { block: id.clone(), start, end, name: "underline", value: None });
    }
    if cs.strike {
        pending.push(PendingMark { block: id.clone(), start, end, name: "strike", value: None });
    }
    if let Some(hex) = color_hex(colors, cs.color) {
        pending.push(PendingMark { block: id.clone(), start, end, name: "color", value: Some(MarkVal::Str(hex)) });
    }
    if let Some(hp) = cs.fs_halfpt {
        let pt = (hp / 2).max(0) as i64;
        pending.push(PendingMark { block: id, start, end, name: "fontSize", value: Some(MarkVal::Int(pt)) });
    }
}

/// Handle a control word: toggle char state, break paragraphs, or note a bullet.
#[allow(clippy::too_many_arguments)]
fn handle_control(
    word: &str,
    param: Option<i32>,
    stack: &mut [CharState],
    skip_depth: &mut Option<usize>,
    depth: usize,
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    need_new_block: &mut bool,
    pending_bullet: &mut bool,
    _colors: &[Option<String>],
    _pending: &mut Vec<PendingMark>,
) {
    // A param of 0 turns a boolean toggle OFF (`\b0`); absent or non-zero = ON.
    let on = param != Some(0);
    let cs = stack.last_mut().expect("scope stack is never empty");

    match word {
        // ---- paragraph breaks --------------------------------------------
        "par" => {
            // Close the current paragraph; the next text opens a fresh one.
            *need_new_block = true;
            *cur = None;
        }
        "pard" => {
            // Reset paragraph-level state. We also treat it as a soft break so a
            // `\pard` between paragraphs starts a new block (docrtf emits one
            // `\pard … \par` per paragraph). Does not reset *character* state.
            *need_new_block = true;
            *cur = None;
            *pending_bullet = false;
        }
        // A line break inside a paragraph → a space (we don't model hard breaks).
        "line" => {
            let id = ensure_block(doc, prev, cur, cur_len, need_new_block, pending_bullet);
            doc.splice_text(&id, *cur_len, 0, " ");
            *cur_len += 1;
        }
        "tab" => {
            let id = ensure_block(doc, prev, cur, cur_len, need_new_block, pending_bullet);
            doc.splice_text(&id, *cur_len, 0, "\t");
            *cur_len += 1;
        }
        // ---- character toggles -------------------------------------------
        "b" => cs.bold = on,
        "i" => cs.italic = on,
        "ul" => cs.underline = on,
        "ulnone" => cs.underline = false,
        "strike" => cs.strike = on,
        "cf" => cs.color = param.unwrap_or(0).max(0) as usize,
        "fs" => cs.fs_halfpt = param,
        // Reset all character formatting (`\plain`).
        "plain" => *cs = CharState::default(),
        // ---- list markers ------------------------------------------------
        // A literal bullet glyph (docrtf emits `\bullet\tab `) marks the
        // paragraph as a list item. `\pntext`/`\pn` groups are list-marker
        // destinations we skip but still flag the bullet.
        "bullet" => *pending_bullet = true,
        "pntext" | "pn" | "listtext" => {
            *pending_bullet = true;
            // The marker content itself is not body text → skip the group.
            *skip_depth = Some(depth);
        }
        // ---- non-content destinations to skip wholesale ------------------
        "fonttbl" | "stylesheet" | "info" | "colortbl" | "pict" | "header" | "footer"
        | "footnote" | "field" | "object" | "themedata" | "datastore" | "generator" => {
            *skip_depth = Some(depth);
        }
        // Everything else (alignment, indents, font selection, doc-level flags,
        // etc.) is intentionally ignored — it doesn't map onto a modeled mark.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::DocSnapshot;
    use crate::Document;

    fn import(src: &str) -> DocSnapshot {
        let mut d = Document::new();
        RtfCodec.import(src.as_bytes(), &mut d.as_mut()).unwrap();
        d.materialize()
    }

    #[test]
    fn detect_recognizes_rtf_signature() {
        assert_eq!(RtfCodec.detect(b"{\\rtf1\\ansi"), Confidence::High);
        assert_eq!(RtfCodec.detect(b"   {\\rtf1"), Confidence::High);
        assert_eq!(RtfCodec.detect(b"not rtf"), Confidence::None);
    }

    #[test]
    fn plain_paragraph_text() {
        let snap = import("{\\rtf1\\ansi\\deff0\n{\\pard\\fs22 Hello world\\par}}");
        let paras: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "paragraph").collect();
        assert_eq!(paras.len(), 1, "{snap:?}");
        assert_eq!(paras[0].text, "Hello world");
    }

    #[test]
    fn two_paragraphs_split_on_par() {
        let snap = import("{\\rtf1\\ansi\n{\\pard First\\par}{\\pard Second\\par}}");
        let texts: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "paragraph").map(|b| b.text.clone()).collect();
        assert_eq!(texts, vec!["First".to_string(), "Second".to_string()]);
    }

    #[test]
    fn bold_run_yields_strong_mark() {
        // `\b … \b0` over a middle run.
        let snap = import("{\\rtf1\\ansi\n{\\pard a{\\b bold}c\\par}}");
        let p = snap.blocks.iter().find(|b| b.kind == "paragraph").expect("paragraph");
        assert_eq!(p.text, "aboldc");
        let strong = p.marks.iter().find(|m| m.name == "strong").expect("strong mark present");
        // The bold run is "bold" — chars 1..5.
        assert_eq!((strong.start, strong.end), (1, 5), "{:?}", p.marks);
    }

    #[test]
    fn bold_toggle_without_group() {
        // `\b text \b0` — toggle on, then explicitly off, same scope.
        let snap = import("{\\rtf1\\ansi\n{\\pard x\\b on\\b0 y\\par}}");
        let p = snap.blocks.iter().find(|b| b.kind == "paragraph").expect("paragraph");
        assert_eq!(p.text, "xony");
        let strong = p.marks.iter().find(|m| m.name == "strong").expect("strong mark");
        assert_eq!((strong.start, strong.end), (1, 3), "covers 'on': {:?}", p.marks);
    }

    #[test]
    fn italic_underline_strike_marks() {
        let snap = import("{\\rtf1\\ansi\n{\\pard {\\i it}{\\ul un}{\\strike st}\\par}}");
        let p = snap.blocks.iter().find(|b| b.kind == "paragraph").expect("paragraph");
        assert_eq!(p.text, "itunst");
        assert!(p.marks.iter().any(|m| m.name == "em" && m.start == 0 && m.end == 2), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "underline" && m.start == 2 && m.end == 4), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "strike" && m.start == 4 && m.end == 6), "{:?}", p.marks);
    }

    #[test]
    fn color_table_maps_cf_to_color_mark() {
        // colortbl: index 1 = red. `\cf1 red\cf0` applies it.
        let snap = import(
            "{\\rtf1\\ansi{\\colortbl;\\red255\\green0\\blue0;}\n{\\pard {\\cf1 red}\\par}}",
        );
        let p = snap.blocks.iter().find(|b| b.kind == "paragraph").expect("paragraph");
        assert_eq!(p.text, "red");
        let color = p.marks.iter().find(|m| m.name == "color").expect("color mark");
        assert_eq!(color.value, crate::document::MarkValue::Str("#ff0000".into()), "{:?}", p.marks);
        assert_eq!((color.start, color.end), (0, 3));
    }

    #[test]
    fn fs_maps_to_fontsize_in_points() {
        // `\fs40` = 40 half-points = 20pt.
        let snap = import("{\\rtf1\\ansi\n{\\pard {\\fs40 big}\\par}}");
        let p = snap.blocks.iter().find(|b| b.kind == "paragraph").expect("paragraph");
        let size = p.marks.iter().find(|m| m.name == "fontSize").expect("fontSize mark");
        assert_eq!(size.value, crate::document::MarkValue::Int(20), "{:?}", p.marks);
    }

    #[test]
    fn escapes_and_unicode_decode_to_text() {
        // `\\ \{ \}` literals, a `\'XX` hex byte (é = 0xE9 in Latin-1), and a
        // `\uN?` Unicode escape (✓ = U+2713 = 10003).
        let snap = import("{\\rtf1\\ansi\n{\\pard a\\\\b\\{c\\} \\'e9 \\u10003?z\\par}}");
        let p = snap.blocks.iter().find(|b| b.kind == "paragraph").expect("paragraph");
        assert_eq!(p.text, "a\\b{c} é ✓z");
    }

    #[test]
    fn fonttbl_and_star_destinations_are_skipped() {
        // The font table and a `\*\generator` destination must not leak into body.
        let snap = import(
            "{\\rtf1\\ansi{\\fonttbl{\\f0 Helvetica;}}{\\*\\generator qOS;}\n{\\pard Body\\par}}",
        );
        let texts: Vec<_> = snap.blocks.iter().map(|b| b.text.clone()).collect();
        assert_eq!(texts, vec!["Body".to_string()], "only body text survives: {snap:?}");
    }

    #[test]
    fn bullet_paragraph_becomes_list_item() {
        let snap = import("{\\rtf1\\ansi\n{\\pard\\bullet\\tab Item one\\par}}");
        let li = snap.blocks.iter().find(|b| b.kind == "list_item").expect("list_item block");
        assert!(li.text.contains("Item one"), "got {:?}", li.text);
    }

    #[test]
    fn export_is_unsupported() {
        let d = Document::new();
        let err = RtfCodec.export(d.as_ref()).unwrap_err();
        assert!(matches!(err, CodecError::Unsupported(_)), "got {err:?}");
    }

    /// Round-trip sanity over a hand-written RTF snapshot shaped exactly like
    /// `docrtf`'s emit side (`{\pard\fs22 …{\b run}…\par}`): the text and the
    /// bold run must both survive import. (The full `docrtf`-driven round-trip
    /// lives in the `editor` crate, which depends on both `docrtf` and `qdoc` —
    /// `qdoc` cannot depend on `docrtf` without a dependency cycle.)
    #[test]
    fn roundtrip_docrtf_shaped_rtf_preserves_text_and_bold() {
        // The exact shape `docrtf::render_rtf` produces for "Hello **world** today".
        let rtf = "{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0\\fswiss Helvetica;}}{\\colortbl;}\n\
{\\pard\\fs22 {\\fs22 Hello }{\\b\\fs22 world}{\\fs22  today}\\par}}";
        let snap = import(rtf);
        let p = snap.blocks.iter().find(|b| b.kind == "paragraph").expect("a paragraph survives");
        assert_eq!(p.text, "Hello world today", "text round-trips: {snap:?}");
        let strong = p.marks.iter().find(|m| m.name == "strong").expect("bold run survives");
        // "world" sits at chars 6..11.
        assert_eq!(&p.text[strong.start..strong.end], "world", "bold covers the right run: {:?}", p.marks);
    }
}
