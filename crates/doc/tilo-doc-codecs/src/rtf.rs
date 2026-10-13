//! Built-in RTF **import** codec — parses `{\rtf1 …}` back onto the canonical
//! rich-text model ([`crate::export_rtf`] handles export). Groups `{ … }` push/pop
//! formatting state; control words `\word`/`\wordN` take an optional signed
//! parameter; control symbols cover literal specials (`\\ \{ \}`), `\'XX` hex
//! bytes, `\uN` Unicode units, and `\*` (skip-destination).
//!
//! Control word → model: `\par`/`\pard` close the paragraph (`\pard` also
//! resets paragraph state); `\b`/`\i`/`\ul`/`\strike` (+ `0`/`none` forms) →
//! `strong`/`em`/`underline`/`strike`; `\cfN` → `color` (0 = auto); `\fsN` →
//! `fontSize` in points (`N` is half-points); a bulleted paragraph →
//! `list_item`. `\fonttbl`/`\stylesheet`/`\info`/`\*\…` are skipped; `\colortbl`
//! is parsed for the color lookup; escapes decode to literal text.
//!
//! RTF has no semantic headings, so every paragraph maps to `paragraph` (or
//! `list_item` if bulleted). `export` is `Unsupported`: RTF export is
//! [`crate::export_rtf::render_rtf`] (needs a materialized snapshot a
//! `FormatProvider` doesn't get).
use crate::format::{CodecError, FormatProvider};
use tilo_content::{ContentInputs, ContentSource};
use tilo_doc::model::{DocMut, DocRef};

pub struct RtfCodec;

impl FormatProvider for RtfCodec {
    fn import(
        &self,
        bytes: &[u8],
        doc: &mut DocMut,
        _inputs: &mut ContentInputs,
    ) -> Result<(), CodecError> {
        doc.bulk(|doc| {
            // RTF is ASCII-with-escapes; non-ASCII only appears inside `\'XX`/`\uN`, which
            // we decode ourselves — map each byte to a char (Latin-1-ish) so char index == byte index.
            let src: String = bytes.iter().map(|&b| b as char).collect();
            import_rtf(&src, doc);
            Ok(())
        })
    }

    fn export(&self, _doc: DocRef, _content: &dyn ContentSource) -> Result<Vec<u8>, CodecError> {
        // The RTF writer renders a materialized `DocSnapshot`, not a `DocRef`.
        Err(CodecError::Unsupported(
            "RTF export is provided by tilo_doc::export_rtf::render_rtf, not the qdoc codec"
                .to_string(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    GroupOpen,
    GroupClose,
    Control(String, Option<i32>),
    /// A control symbol: a single non-alpha char after `\` (`\\ \{ \}`, `\*`, `\~`, `\-`, …); `\'XX`/`\uN` lex as their own tokens.
    Symbol(char),
    /// A `\'XX` hex byte escape → the decoded byte value.
    HexByte(u8),
    /// A `\uN` Unicode escape → the signed 16-bit code unit `N`.
    Unicode(i32),
    Text(String),
}

/// Char at `i`, or `'\0'` past the end. Every call site here is guarded by `i < n` first, so
/// the sentinel is never actually observed — it exists so indexing can't panic on the
/// (unreachable) alternative.
fn at(chars: &[char], i: usize) -> char {
    chars.get(i).copied().unwrap_or('\0')
}

/// Tokenizes the RTF source over chars (== bytes after the import-side byte→char map).
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
        let c = at(&chars, i);
        match c {
            '{' => {
                flush_text!();
                out.push(Token::GroupOpen);
                i = i.saturating_add(1);
            }
            '}' => {
                flush_text!();
                out.push(Token::GroupClose);
                i = i.saturating_add(1);
            }
            '\r' | '\n' => {
                i = i.saturating_add(1);
            }
            '\\' => {
                flush_text!();
                i = i.saturating_add(1);
                if i >= n {
                    break;
                }
                let d = at(&chars, i);
                if d.is_ascii_alphabetic() {
                    let mut word = String::new();
                    while i < n && at(&chars, i).is_ascii_alphabetic() {
                        word.push(at(&chars, i));
                        i = i.saturating_add(1);
                    }
                    let param = if i < n && (at(&chars, i) == '-' || at(&chars, i).is_ascii_digit())
                    {
                        let mut num = String::new();
                        if at(&chars, i) == '-' {
                            num.push('-');
                            i = i.saturating_add(1);
                        }
                        while i < n && at(&chars, i).is_ascii_digit() {
                            num.push(at(&chars, i));
                            i = i.saturating_add(1);
                        }
                        num.parse::<i32>().ok()
                    } else {
                        None
                    };
                    // A single trailing space is the control-word delimiter and is consumed.
                    if i < n && at(&chars, i) == ' ' {
                        i = i.saturating_add(1);
                    }
                    // `\u`: the parameter is the code unit; RTF's `\ucN` governs how many fallback chars follow (default 1) — we always skip exactly one.
                    if word == "u" {
                        if let Some(cu) = param {
                            out.push(Token::Unicode(cu));
                            i = skip_one_fallback(&chars, i, n);
                        }
                    } else {
                        out.push(Token::Control(word, param));
                    }
                } else if d == '\'' {
                    i = i.saturating_add(1);
                    let mut hex = String::new();
                    while i < n && hex.len() < 2 && at(&chars, i).is_ascii_hexdigit() {
                        hex.push(at(&chars, i));
                        i = i.saturating_add(1);
                    }
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        out.push(Token::HexByte(byte));
                    }
                } else {
                    // The single char is the symbol; `\\ \{ \}` are literal specials, others (`\*`, `\~`, `\-`, …) carry meaning the parser interprets.
                    out.push(Token::Symbol(d));
                    i = i.saturating_add(1);
                }
            }
            _ => {
                text.push(c);
                i = i.saturating_add(1);
            }
        }
    }
    flush_text!();
    out
}

/// Skips one Unicode-fallback unit after `\uN`: a literal char, or a whole `\'XX`/
/// control-word substitute. Assumes RTF's default `\ucN 1`; a custom `\uc` isn't tracked.
fn skip_one_fallback(chars: &[char], mut i: usize, n: usize) -> usize {
    if i >= n {
        return i;
    }
    if at(chars, i) == '\\' {
        i = i.saturating_add(1);
        if i < n && at(chars, i) == '\'' {
            i = i.saturating_add(1);
            let mut k: usize = 0;
            while i < n && k < 2 && at(chars, i).is_ascii_hexdigit() {
                i = i.saturating_add(1);
                k = k.saturating_add(1);
            }
        } else {
            while i < n && at(chars, i).is_ascii_alphabetic() {
                i = i.saturating_add(1);
            }
        }
    } else {
        i = i.saturating_add(1); // a single literal char
    }
    i
}

/// Parses a `{\colortbl;\redR\greenG\blueB; …}` group into a 1-based list of `#rrggbb` strings; index 0 is the reserved auto entry.
fn parse_color_table(tokens: &[Token]) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = Vec::new();
    let (mut red, mut green, mut blue) = (0u32, 0u32, 0u32);
    let mut saw_component = false;
    for tok in tokens {
        match tok {
            Token::Control(word, param) => {
                let component = u32::try_from(param.unwrap_or(0).clamp(0, 255)).unwrap_or(0);
                match word.as_str() {
                    "red" => {
                        red = component;
                        saw_component = true;
                    }
                    "green" => {
                        green = component;
                        saw_component = true;
                    }
                    "blue" => {
                        blue = component;
                        saw_component = true;
                    }
                    _ => {}
                }
            }
            Token::Text(s) => {
                for ch in s.chars() {
                    if ch == ';' {
                        if saw_component {
                            out.push(Some(format!("#{red:02x}{green:02x}{blue:02x}")));
                        } else {
                            out.push(None); // the reserved/auto slot
                        }
                        red = 0;
                        green = 0;
                        blue = 0;
                        saw_component = false;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Per-scope character formatting, pushed/popped on `{`/`}`.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent, orthogonal per-run toggles mirroring RTF's own flags (\\b \\i \\ul \\strike); a bitflags enum would add indirection with no behavioral benefit"
)]
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

/// A run mark gathered during parsing, applied after the block's text is complete (so `ExpandMark::After` doesn't stretch it over trailing text).
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

/// If the token at `idx` opens a `\colortbl` group, parses it and returns `(colors, index just
/// past the closing brace)`.
fn consume_colortbl(tokens: &[Token], idx: usize) -> Option<(Vec<Option<String>>, usize)> {
    let start = idx.saturating_add(1);
    let mut j = start;
    let mut inner_depth = 1usize; // we are inside the colortbl's group
    while j < tokens.len() && inner_depth > 0 {
        match tokens.get(j) {
            Some(Token::GroupOpen) => inner_depth = inner_depth.saturating_add(1),
            Some(Token::GroupClose) => inner_depth = inner_depth.saturating_sub(1),
            _ => {}
        }
        if inner_depth == 0 {
            break;
        }
        j = j.saturating_add(1);
    }
    let colors = parse_color_table(tokens.get(start..j)?);
    Some((colors, j))
}

/// The block being filled, its char length, and whether the *next* text should open a fresh
/// paragraph (true initially and after every `\par`).
struct BlockCursor {
    prev: Option<String>,
    cur: Option<String>,
    cur_len: usize,
    need_new_block: bool,
    /// Whether the pending/just-started paragraph is a list item (saw a bullet).
    pending_bullet: bool,
}

impl Default for BlockCursor {
    fn default() -> Self {
        Self {
            prev: None,
            cur: None,
            cur_len: 0,
            need_new_block: true,
            pending_bullet: false,
        }
    }
}

impl BlockCursor {
    /// Open a fresh `paragraph` (or `list_item`) block and make it current.
    fn open_block(&mut self, doc: &mut DocMut, is_list: bool) {
        let kind = if is_list { "list_item" } else { "paragraph" };
        let id = doc.insert_block(self.prev.as_deref(), kind);
        if is_list {
            doc.set_attr(&id, "ordered", false);
        }
        self.prev = Some(id.clone());
        self.cur = Some(id);
        self.cur_len = 0;
    }

    /// Ensures a current block exists (opening one if a paragraph break is pending), returning its id.
    fn ensure_block(&mut self, doc: &mut DocMut) -> String {
        if self.need_new_block || self.cur.is_none() {
            self.open_block(doc, self.pending_bullet);
            self.need_new_block = false;
            self.pending_bullet = false;
        }
        // `cur` is always `Some` here — either it already was, or `open_block` just set it
        // unconditionally above — so the empty-string fallback is never actually observed.
        self.cur.clone().unwrap_or_default()
    }

    fn push_char(&mut self, doc: &mut DocMut, ch: char) {
        let id = self.ensure_block(doc);
        let mut buf = [0u8; 4];
        doc.splice_text(&id, self.cur_len, 0, ch.encode_utf8(&mut buf));
        self.cur_len = self.cur_len.saturating_add(1);
    }

    /// Handle a control symbol by emitting its literal char, if any (`\*` is handled by the
    /// caller, since it opens a skip-destination rather than producing content).
    fn push_symbol(&mut self, doc: &mut DocMut, sym: char) {
        let literal = match sym {
            '\\' => '\\',
            '{' => '{',
            '}' => '}',
            '~' => '\u{a0}',
            '_' => '\u{2011}',
            // Optional hyphen (no visible glyph, so falls through here too) and
            // anything else unrecognized.
            _ => return,
        };
        self.push_char(doc, literal);
    }
}

/// Everything [`import_rtf`]'s token loop carries from one token to the next.
struct RtfImport {
    /// Per-scope character formatting, pushed/popped on `{`/`}`.
    stack: Vec<CharState>,
    colors: Vec<Option<String>>,
    cursor: BlockCursor,
    pending_marks: Vec<PendingMark>,
    /// Skip-destination depth: entering an ignorable destination (`\*`, or a known
    /// non-content one) drops tokens until its closing brace (`None` = not skipping).
    skip_depth: Option<usize>,
    depth: usize,
}

impl RtfImport {
    fn new() -> Self {
        Self {
            stack: vec![CharState::default()],
            colors: Vec::new(),
            cursor: BlockCursor::default(),
            pending_marks: Vec::new(),
            skip_depth: None,
            depth: 0,
        }
    }

    /// Route one token to its content-producing handler, honoring `skip_depth`. `depth` still
    /// advances/retreats on group tokens even while skipping, so the skip-destination's own
    /// closing brace is still detected.
    fn dispatch_token(&mut self, tok: &Token, doc: &mut DocMut) {
        let skipping = self.skip_depth.is_some();
        match tok {
            Token::GroupOpen => {
                self.depth = self.depth.saturating_add(1);
                let top = self.stack.last().cloned().unwrap_or_default();
                self.stack.push(top);
            }
            Token::GroupClose => {
                if self.stack.len() > 1 {
                    self.stack.pop();
                }
                if let Some(sd) = self.skip_depth
                    && self.depth == sd
                {
                    self.skip_depth = None;
                }
                self.depth = self.depth.saturating_sub(1);
            }
            Token::Symbol(sym) if !skipping => {
                if *sym == '*' {
                    self.skip_depth = Some(self.depth);
                } else {
                    self.cursor.push_symbol(doc, *sym);
                }
            }
            Token::Control(word, param) if !skipping => self.handle_control(word, *param, doc),
            Token::HexByte(b) if !skipping => {
                // Decode the byte as Latin-1 (the common ANSI fallback): most documents using a
                // different code page still escape ASCII as ASCII, so this is correct in the printable range.
                self.cursor.push_char(doc, *b as char);
            }
            Token::Unicode(cu) if !skipping => {
                let u = u16::try_from(i64::from(*cu) & 0xffff).unwrap_or(0);
                let ch = char::from_u32(u32::from(u)).unwrap_or('\u{fffd}');
                self.cursor.push_char(doc, ch);
            }
            Token::Text(s) if !skipping => self.push_text(s, doc),
            // Currently inside a skip-destination: drop the token's content.
            Token::Symbol(_)
            | Token::Control(..)
            | Token::HexByte(_)
            | Token::Unicode(_)
            | Token::Text(_) => {}
        }
    }

    /// Pushes a run of literal text under the active char formatting, gathering the run marks it implies.
    fn push_text(&mut self, s: &str, doc: &mut DocMut) {
        let cs = self.stack.last().cloned().unwrap_or_default();
        if s.is_empty() {
            return;
        }
        let id = self.cursor.ensure_block(doc);
        let start = self.cursor.cur_len;
        doc.splice_text(&id, start, 0, s);
        let len = s.chars().count();
        let end = start.saturating_add(len);
        self.cursor.cur_len = end;
        let pending_marks = &mut self.pending_marks;

        if cs.bold {
            pending_marks.push(PendingMark {
                block: id.clone(),
                start,
                end,
                name: "strong",
                value: None,
            });
        }
        if cs.italic {
            pending_marks.push(PendingMark {
                block: id.clone(),
                start,
                end,
                name: "em",
                value: None,
            });
        }
        if cs.underline {
            pending_marks.push(PendingMark {
                block: id.clone(),
                start,
                end,
                name: "underline",
                value: None,
            });
        }
        if cs.strike {
            pending_marks.push(PendingMark {
                block: id.clone(),
                start,
                end,
                name: "strike",
                value: None,
            });
        }
        if let Some(hex) = color_hex(&self.colors, cs.color) {
            pending_marks.push(PendingMark {
                block: id.clone(),
                start,
                end,
                name: "color",
                value: Some(MarkVal::Str(hex)),
            });
        }
        if let Some(hp) = cs.fs_halfpt {
            let pt = i64::from((hp / 2).max(0));
            pending_marks.push(PendingMark {
                block: id,
                start,
                end,
                name: "fontSize",
                value: Some(MarkVal::Int(pt)),
            });
        }
    }

    /// Handle a control word: toggle char state, break paragraphs, or note a bullet.
    fn handle_control(&mut self, word: &str, param: Option<i32>, doc: &mut DocMut) {
        // A param of 0 turns a boolean toggle OFF (`\b0`); absent or non-zero = ON.
        let on = param != Some(0);
        let cursor = &mut self.cursor;
        // `stack` always holds at least one `CharState`: seeded with one in `RtfImport::new`, and
        // `GroupClose` only pops while `len() > 1` — but a control word on an empty stack is a
        // no-op rather than a panic if that invariant is ever broken.
        let Some(cs) = self.stack.last_mut() else {
            return;
        };

        match word {
            "par" => {
                cursor.need_new_block = true;
                cursor.cur = None;
            }
            "pard" => {
                // Reset paragraph-level state; also a soft break so `\pard` between paragraphs
                // starts a new block (`export_rtf` emits one `\pard … \par` per paragraph); doesn't reset char state.
                cursor.need_new_block = true;
                cursor.cur = None;
                cursor.pending_bullet = false;
            }
            "line" => {
                let id = cursor.ensure_block(doc);
                doc.splice_text(&id, cursor.cur_len, 0, " ");
                cursor.cur_len = cursor.cur_len.saturating_add(1);
            }
            "tab" => {
                let id = cursor.ensure_block(doc);
                doc.splice_text(&id, cursor.cur_len, 0, "\t");
                cursor.cur_len = cursor.cur_len.saturating_add(1);
            }
            "b" => cs.bold = on,
            "i" => cs.italic = on,
            "ul" => cs.underline = on,
            "ulnone" => cs.underline = false,
            "strike" => cs.strike = on,
            "cf" => cs.color = usize::try_from(param.unwrap_or(0).max(0)).unwrap_or(0),
            "fs" => cs.fs_halfpt = param,
            "plain" => *cs = CharState::default(),
            // A literal bullet glyph (`export_rtf` emits `\bullet\tab `) marks the paragraph as a
            // list item; `\pntext`/`\pn` groups are list-marker destinations we skip but still flag it.
            "bullet" => cursor.pending_bullet = true,
            "pntext" | "pn" | "listtext" => {
                cursor.pending_bullet = true;
                self.skip_depth = Some(self.depth);
            }
            "fonttbl" | "stylesheet" | "info" | "colortbl" | "pict" | "header" | "footer"
            | "footnote" | "field" | "object" | "themedata" | "datastore" | "generator" => {
                self.skip_depth = Some(self.depth);
            }
            // Everything else (alignment, indents, font selection, doc-level flags, etc.) is intentionally ignored — it doesn't map onto a modeled mark.
            _ => {}
        }
    }
}

fn apply_pending_marks(doc: &mut DocMut, pending_marks: Vec<PendingMark>) {
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

fn import_rtf(src: &str, doc: &mut DocMut) {
    let tokens = tokenize(src);
    let mut st = RtfImport::new();

    let mut idx = 0;
    while idx < tokens.len() {
        let Some(tok) = tokens.get(idx) else { break };

        if st.skip_depth.is_none()
            && let Token::Control(w, _) = tok
            && w == "colortbl"
            && let Some((new_colors, next)) = consume_colortbl(&tokens, idx)
        {
            st.colors = new_colors;
            idx = next;
            continue;
        }

        st.dispatch_token(tok, doc);
        idx = idx.saturating_add(1);
    }

    apply_pending_marks(doc, st.pending_marks);
}

/// Resolve `\cfN` to a `#rrggbb` string, or `None` (auto / out of range).
fn color_hex(colors: &[Option<String>], idx: usize) -> Option<String> {
    if idx == 0 {
        return None;
    }
    colors.get(idx).cloned().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_doc::Document;
    use tilo_doc::snapshot::DocSnapshot;

    fn import(src: &str) -> DocSnapshot {
        let mut d = Document::new();
        RtfCodec
            .import(
                src.as_bytes(),
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        d.materialize()
    }

    #[test]
    fn rtf_import_paragraphs_text_and_destinations() {
        // Phase `plain_paragraph_text`.
        {
            let snap = import("{\\rtf1\\ansi\\deff0\n{\\pard\\fs22 Hello world\\par}}");
            let paras: Vec<_> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "paragraph")
                .collect();
            assert_eq!(paras.len(), 1, "{snap:?}");
            assert_eq!(paras[0].text, "Hello world");
        }
        // Phase `two_paragraphs_split_on_par`.
        {
            let snap = import("{\\rtf1\\ansi\n{\\pard First\\par}{\\pard Second\\par}}");
            let texts: Vec<_> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "paragraph")
                .map(|b| b.text.clone())
                .collect();
            assert_eq!(texts, vec!["First".to_string(), "Second".to_string()]);
        }
        // Phase `escapes_and_unicode_decode_to_text`.
        {
            // literal `\\ \{ \}`, a hex-byte escape (é, Latin-1), and a Unicode escape (checkmark, U+2713).
            let snap = import("{\\rtf1\\ansi\n{\\pard a\\\\b\\{c\\} \\'e9 \\u10003?z\\par}}");
            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("paragraph");
            assert_eq!(p.text, "a\\b{c} é ✓z");
        }
        // Phase `fonttbl_and_star_destinations_are_skipped`.
        {
            // The font table and a `\*\generator` destination must not leak into body.
            let snap = import(
                "{\\rtf1\\ansi{\\fonttbl{\\f0 Helvetica;}}{\\*\\generator Tilo;}\n{\\pard Body\\par}}",
            );
            let texts: Vec<_> = snap.blocks.iter().map(|b| b.text.clone()).collect();
            assert_eq!(
                texts,
                vec!["Body".to_string()],
                "only body text survives: {snap:?}"
            );
        }
        // Phase `bullet_paragraph_becomes_list_item`.
        {
            let snap = import("{\\rtf1\\ansi\n{\\pard\\bullet\\tab Item one\\par}}");
            let li = snap
                .blocks
                .iter()
                .find(|b| b.kind == "list_item")
                .expect("list_item block");
            assert!(li.text.contains("Item one"), "got {:?}", li.text);
        }
        // Phase `export_is_unsupported`.
        {
            let d = Document::new();
            let err = RtfCodec
                .export(d.as_ref(), &tilo_content::NoContent)
                .unwrap_err();
            assert!(matches!(err, CodecError::Unsupported(_)), "got {err:?}");
        }
        // Phase `roundtrip_docrtf_shaped_rtf_preserves_text_and_bold`.
        // A hand-written RTF snapshot shaped like `export_rtf`'s emit side; the full
        // round-trip lives in the word processor's tests.
        {
            let rtf = "{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0\\fswiss Helvetica;}}{\\colortbl;}\n\
    {\\pard\\fs22 {\\fs22 Hello }{\\b\\fs22 world}{\\fs22  today}\\par}}";
            let snap = import(rtf);
            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("a paragraph survives");
            assert_eq!(p.text, "Hello world today", "text round-trips: {snap:?}");
            let strong = p
                .marks
                .iter()
                .find(|m| m.name == "strong")
                .expect("bold run survives");
            assert_eq!(
                p.text
                    .get(strong.start..strong.end)
                    .expect("strong span is within text bounds"),
                "world",
                "bold covers the right run: {:?}",
                p.marks
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn rtf_import_control_words_become_marks() {
        // Phase `bold_run_yields_strong_mark`.
        {
            let snap = import("{\\rtf1\\ansi\n{\\pard a{\\b bold}c\\par}}");
            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("paragraph");
            assert_eq!(p.text, "aboldc");
            let strong = p
                .marks
                .iter()
                .find(|m| m.name == "strong")
                .expect("strong mark present");
            // The bold run is "bold" — chars 1..5.
            assert_eq!((strong.start, strong.end), (1, 5), "{:?}", p.marks);
        }
        // Phase `bold_toggle_without_group`.
        {
            let snap = import("{\\rtf1\\ansi\n{\\pard x\\b on\\b0 y\\par}}");
            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("paragraph");
            assert_eq!(p.text, "xony");
            let strong = p
                .marks
                .iter()
                .find(|m| m.name == "strong")
                .expect("strong mark");
            assert_eq!(
                (strong.start, strong.end),
                (1, 3),
                "covers 'on': {:?}",
                p.marks
            );
        }
        // Phase `italic_underline_strike_marks`.
        {
            let snap = import("{\\rtf1\\ansi\n{\\pard {\\i it}{\\ul un}{\\strike st}\\par}}");
            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("paragraph");
            assert_eq!(p.text, "itunst");
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "em" && m.start == 0 && m.end == 2),
                "{:?}",
                p.marks
            );
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "underline" && m.start == 2 && m.end == 4),
                "{:?}",
                p.marks
            );
            assert!(
                p.marks
                    .iter()
                    .any(|m| m.name == "strike" && m.start == 4 && m.end == 6),
                "{:?}",
                p.marks
            );
        }
        // Phase `color_table_maps_cf_to_color_mark`.
        {
            // colortbl: index 1 = red. `\cf1 red\cf0` applies it.
            let snap = import(
                "{\\rtf1\\ansi{\\colortbl;\\red255\\green0\\blue0;}\n{\\pard {\\cf1 red}\\par}}",
            );
            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("paragraph");
            assert_eq!(p.text, "red");
            let color = p
                .marks
                .iter()
                .find(|m| m.name == "color")
                .expect("color mark");
            assert_eq!(
                color.value,
                tilo_doc::document::MarkValue::Str("#ff0000".into()),
                "{:?}",
                p.marks
            );
            assert_eq!((color.start, color.end), (0, 3));
        }
        // Phase `fs_maps_to_fontsize_in_points`.
        {
            // `\fs40` = 40 half-points = 20pt.
            let snap = import("{\\rtf1\\ansi\n{\\pard {\\fs40 big}\\par}}");
            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("paragraph");
            let size = p
                .marks
                .iter()
                .find(|m| m.name == "fontSize")
                .expect("fontSize mark");
            assert_eq!(
                size.value,
                tilo_doc::document::MarkValue::Int(20),
                "{:?}",
                p.marks
            );
        }
    }
}
