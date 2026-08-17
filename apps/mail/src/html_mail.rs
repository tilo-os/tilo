//! Native, sanitized rendering of an email's `text/html` body.
//!
//! An email's `text/html` part is hostile input: it can carry scripts, tracking
//! pixels, remote images that phone home the instant the mail is opened, and web
//! fonts that leak the read event. Rather than hand that to a web engine, we
//! **parse the HTML into a small, safe block model** ([`parse`]) and paint it
//! with the shared `ui` text + image primitives ([`draw_body`]).
//!
//! This is safe by construction and legible everywhere:
//!
//! * **No scripting, no CSS execution.** `<script>`/`<style>`/`<head>` are
//!   dropped wholesale; only structural tags and a handful of inline emphasis
//!   tags survive. There is no code path from mail markup into behaviour.
//! * **Links are sanitized** — only `http`/`https`/`mailto` targets are kept;
//!   anything else (e.g. `javascript:`) renders as inert text.
//! * **Remote content is blocked by default.** Inline `data:` images always
//!   render (their bytes are already in the mail); a remote (`http(s):`) image
//!   loads **only** once the reader opts in — until then it is a labelled
//!   placeholder, never a silent network hit. 1×1 tracking pixels are dropped
//!   entirely.
//! * **It renders in every build** — the default app, headless `--shots`, and
//!   tests — because it needs no engine, only the toolkit's text/image layers.

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{BlendMode, Fill, ImageBrush};

use ui::ImageStore;
use ui::icons::Icon;
use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// One inline run of text within a block, carrying its emphasis and an optional
/// (already-sanitized) link target.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// A sanitized `http`/`https`/`mailto` target, or `None` for plain text.
    pub link: Option<String>,
}

/// A block-level element of a parsed HTML mail body.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// `<h1>`…`<h6>`, with the level (1–6).
    Heading(u8, Vec<Span>),
    /// `<p>` / `<div>` / bare text.
    Paragraph(Vec<Span>),
    /// A `<li>`; `Some(n)` for an ordered list's number, `None` for a bullet.
    ListItem(Option<usize>, Vec<Span>),
    /// `<blockquote>`.
    Quote(Vec<Span>),
    /// `<img>`. `w`/`h` carry the declared pixel dimensions when present (used
    /// to drop tracking pixels and to reserve layout height before load).
    Image { src: String, alt: String, w: Option<u32>, h: Option<u32> },
    /// `<hr>`.
    Rule,
}

// ============================== Parsing ===================================

/// The inline emphasis + link state carried down the tag stack.
#[derive(Clone, Default)]
struct InlineStyle {
    bold: u32,
    italic: u32,
    link: Option<String>,
}

/// The block currently being accumulated.
#[derive(Clone)]
enum Ctx {
    Paragraph,
    Heading(u8),
    Quote,
    ListItem(Option<usize>),
}

/// Parse + sanitize an HTML mail body into a renderable block list.
pub fn parse(html: &str) -> Vec<Block> {
    let mut p = Parser::default();
    p.run(html);
    p.finish()
}

#[derive(Default)]
struct Parser {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    text: String,
    style: InlineStyle,
    ctx: Option<Ctx>,
    /// Nesting of list types: `true` = ordered, plus the running item counter.
    lists: Vec<(bool, usize)>,
    /// Depth of an element whose content is dropped wholesale (script/style/head).
    skip: u32,
}

impl Parser {
    fn run(&mut self, html: &str) {
        let bytes = html.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'<' {
                // Find the tag's end (`>`). Malformed (no close) → treat the
                // rest as text so nothing is silently swallowed.
                if let Some(end) = html[i + 1..].find('>') {
                    let raw = &html[i + 1..i + 1 + end];
                    self.tag(raw);
                    i += 1 + end + 1;
                    continue;
                }
            }
            // Accumulate a run of text up to the next `<`.
            let next = html[i..].find('<').map(|d| i + d).unwrap_or(bytes.len());
            if self.skip == 0 {
                self.text.push_str(&html[i..next]);
            }
            i = next;
        }
    }

    fn tag(&mut self, raw: &str) {
        let raw = raw.trim();
        if raw.is_empty() {
            return;
        }
        // Comments / doctype / processing instructions: ignore.
        if raw.starts_with('!') || raw.starts_with('?') {
            return;
        }
        let closing = raw.starts_with('/');
        let body = if closing { &raw[1..] } else { raw };
        let name = body
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.is_empty() {
            return;
        }

        // Dropped subtrees: everything between <script>…</script> etc. is gone.
        if matches!(name.as_str(), "script" | "style" | "head" | "title") {
            if closing {
                self.skip = self.skip.saturating_sub(1);
            } else if !raw.ends_with('/') {
                self.skip += 1;
            }
            return;
        }
        if self.skip > 0 {
            return;
        }

        match name.as_str() {
            // Block boundaries flush the current inline run.
            "p" | "div" | "section" | "article" | "header" | "footer" | "table"
            | "tr" | "center" => {
                self.flush_block();
                if !closing {
                    self.ctx = Some(Ctx::Paragraph);
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush_block();
                if !closing {
                    let level = name.as_bytes()[1] - b'0';
                    self.ctx = Some(Ctx::Heading(level));
                }
            }
            "blockquote" => {
                self.flush_block();
                if !closing {
                    self.ctx = Some(Ctx::Quote);
                }
            }
            "ul" | "ol" => {
                self.flush_block();
                if closing {
                    self.lists.pop();
                } else {
                    self.lists.push((name == "ol", 0));
                }
            }
            "li" => {
                self.flush_block();
                if !closing {
                    let marker = match self.lists.last_mut() {
                        Some((true, n)) => {
                            *n += 1;
                            Some(*n)
                        }
                        _ => None,
                    };
                    self.ctx = Some(Ctx::ListItem(marker));
                }
            }
            "br" => {
                self.text.push('\n');
            }
            "hr" => {
                self.flush_block();
                self.blocks.push(Block::Rule);
            }
            // Inline emphasis.
            "b" | "strong" => self.emphasis(closing, true, false),
            "i" | "em" => self.emphasis(closing, false, true),
            "a" => {
                if closing {
                    self.push_run();
                    self.style.link = None;
                } else {
                    self.push_run();
                    self.style.link = attr(body, "href").and_then(|h| sanitize_link(&h));
                }
            }
            "img" => {
                if let Some(src) = attr(body, "src").and_then(|s| sanitize_img_src(&s)) {
                    self.push_run();
                    let alt = attr(body, "alt").map(|a| decode_entities(&a)).unwrap_or_default();
                    let w = attr(body, "width").and_then(|v| v.trim().parse().ok());
                    let h = attr(body, "height").and_then(|v| v.trim().parse().ok());
                    // Tracking pixels (1×1 or zero-area) carry nothing to show.
                    if w == Some(0) || h == Some(0) || (w == Some(1) && h == Some(1)) {
                        return;
                    }
                    // An image breaks the current block: flush what precedes it.
                    self.flush_block();
                    self.blocks.push(Block::Image { src, alt, w, h });
                }
            }
            _ => {}
        }
    }

    fn emphasis(&mut self, closing: bool, bold: bool, italic: bool) {
        self.push_run();
        let slot = if bold { &mut self.style.bold } else { &mut self.style.italic };
        let _ = italic;
        if closing {
            *slot = slot.saturating_sub(1);
        } else {
            *slot += 1;
        }
    }

    /// Close the pending text as a span (decoding entities + collapsing runs of
    /// whitespace, but *keeping* edge spaces so adjacent inline runs stay
    /// word-separated), preserving the current inline style.
    fn push_run(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let decoded = decode_entities(&self.text);
        self.text.clear();
        let collapsed = collapse_ws(&decoded);
        if collapsed.is_empty() {
            return;
        }
        self.spans.push(Span {
            text: collapsed,
            bold: self.style.bold > 0,
            italic: self.style.italic > 0,
            link: self.style.link.clone(),
        });
    }

    /// Emit the accumulated spans as a block of the current context, then reset.
    fn flush_block(&mut self) {
        self.push_run();
        normalize_spans(&mut self.spans);
        if self.spans.is_empty() {
            self.ctx = None;
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        let block = match self.ctx.take() {
            Some(Ctx::Heading(level)) => Block::Heading(level, spans),
            Some(Ctx::Quote) => Block::Quote(spans),
            Some(Ctx::ListItem(marker)) => Block::ListItem(marker, spans),
            _ => Block::Paragraph(spans),
        };
        self.blocks.push(block);
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush_block();
        self.blocks
    }
}

/// The value of attribute `key` in a tag body, unquoted. Case-insensitive key.
fn attr(tag_body: &str, key: &str) -> Option<String> {
    let lower = tag_body.to_ascii_lowercase();
    let mut from = 0;
    loop {
        let rel = lower[from..].find(key)?;
        let at = from + rel;
        // Must be preceded by whitespace (or be at the very start after the tag
        // name) and followed by `=` — so `href` doesn't match inside another
        // attribute's value.
        let before_ok = at > 0 && lower.as_bytes()[at - 1].is_ascii_whitespace();
        let after = at + key.len();
        let eq = lower[after..].trim_start().starts_with('=');
        if before_ok && eq {
            let mut rest = tag_body[after..].trim_start();
            rest = rest.strip_prefix('=')?.trim_start();
            let val = if let Some(stripped) = rest.strip_prefix('"') {
                stripped.split('"').next().unwrap_or("")
            } else if let Some(stripped) = rest.strip_prefix('\'') {
                stripped.split('\'').next().unwrap_or("")
            } else {
                rest.split(|c: char| c.is_whitespace()).next().unwrap_or("")
            };
            return Some(val.to_string());
        }
        from = at + key.len();
        if from >= lower.len() {
            return None;
        }
    }
}

/// Keep only safe link schemes; everything else (notably `javascript:`) is
/// dropped so the anchor renders as inert text.
fn sanitize_link(href: &str) -> Option<String> {
    let h = decode_entities(href.trim());
    let lower = h.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
    {
        Some(h)
    } else {
        None
    }
}

/// Keep only safe image sources: inline `data:` (bytes already in the mail) and
/// remote `http(s):` (gated at render time by the remote-image opt-in).
fn sanitize_img_src(src: &str) -> Option<String> {
    let s = decode_entities(src.trim());
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("data:image/")
        || lower.starts_with("http://")
        || lower.starts_with("https://")
    {
        Some(s)
    } else {
        None
    }
}

/// Collapse every run of ASCII whitespace to a single space — HTML's whitespace
/// model for flow content — while **keeping** a single leading/trailing space so
/// two adjacent inline runs (e.g. text around `<strong>`) stay word-separated.
/// Block-edge trimming happens later in [`normalize_spans`]. Explicit `<br>`
/// newlines survive.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        if c == '\n' {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
            pending_space = false;
        } else if c.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space && !out.ends_with('\n') {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        }
    }
    // A trailing (or wholly-whitespace) run keeps one space so it still
    // separates the neighbouring inline runs; block edges are trimmed later.
    if pending_space && !out.ends_with('\n') {
        out.push(' ');
    }
    out
}

/// Tidy a block's inline runs after collapsing: drop a double space at a run
/// boundary (trailing space of one run + leading space of the next), trim the
/// block's leading/trailing space, and drop any run left empty.
fn normalize_spans(spans: &mut Vec<Span>) {
    for i in 1..spans.len() {
        if spans[i - 1].text.ends_with([' ', '\n']) {
            while spans[i].text.starts_with(' ') {
                spans[i].text.remove(0);
            }
        }
    }
    if let Some(first) = spans.first_mut() {
        let trimmed = first.text.trim_start().to_string();
        first.text = trimmed;
    }
    if let Some(last) = spans.last_mut() {
        let trimmed = last.text.trim_end().to_string();
        last.text = trimmed;
    }
    spans.retain(|s| !s.text.is_empty());
}

/// Decode the HTML entities that actually turn up in mail (`&amp;`, `&lt;`,
/// `&#39;`, `&#x2192;`, `&nbsp;`, …). Unknown entities are passed through
/// verbatim so no text is lost.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(semi) = s[i + 1..].find(';') {
                let ent = &s[i + 1..i + 1 + semi];
                if let Some(ch) = decode_entity(ent) {
                    out.push(ch);
                    i += 1 + semi + 1;
                    continue;
                }
            }
        }
        // Not a recognised entity: copy this char through.
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn decode_entity(ent: &str) -> Option<char> {
    if let Some(num) = ent.strip_prefix('#') {
        let code = if let Some(hex) = num.strip_prefix(['x', 'X']) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return char::from_u32(code);
    }
    Some(match ent {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        "nbsp" => '\u{00a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "rsquo" => '\u{2019}',
        "lsquo" => '\u{2018}',
        "rdquo" => '\u{201d}',
        "ldquo" => '\u{201c}',
        "middot" => '·',
        "bull" => '•',
        "rarr" => '→',
        "larr" => '←',
        _ => return None,
    })
}

// ============================ data: URIs ==================================

/// Decode a `data:` URI's payload into raw bytes (for the reader's image store
/// fetcher). Handles base64 and percent/plain text bodies. Returns `None` for a
/// non-`data:` URI so the caller can fall through to a network fetch.
pub fn decode_data_uri(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("data:").or_else(|| uri.strip_prefix("DATA:"))?;
    let comma = rest.find(',')?;
    let meta = &rest[..comma];
    let payload = &rest[comma + 1..];
    if meta.to_ascii_lowercase().split(';').any(|p| p == "base64") {
        base64_decode(payload)
    } else {
        // Percent-decoded text payload.
        Some(percent_decode(payload))
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Minimal standard base64 (RFC 4648) decoder — whitespace is skipped.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let filtered: Vec<u8> = s.bytes().filter(|&c| c != b'=' && !c.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(filtered.len() / 4 * 3);
    for chunk in filtered.chunks(4) {
        let mut n = 0u32;
        let mut cnt = 0;
        for &c in chunk {
            n = (n << 6) | val(c)?;
            cnt += 1;
        }
        n <<= 6 * (4 - cnt);
        if cnt >= 2 {
            out.push((n >> 16) as u8);
        }
        if cnt >= 3 {
            out.push((n >> 8) as u8);
        }
        if cnt >= 4 {
            out.push(n as u8);
        }
    }
    Some(out)
}

// ============================== Rendering =================================

/// Body font size (logical px) for paragraph text.
const BODY_SIZE: f32 = 14.0;

fn rgb(c: vello::peniko::Color) -> [u8; 3] {
    let a = c.to_rgba8();
    [a.r, a.g, a.b]
}

/// The result of painting a body: total (unclipped) content height, the link
/// hit-rects (absolute coords), and whether an opted-in image is still loading.
#[derive(Default)]
pub struct Rendered {
    pub content_height: f64,
    pub links: Vec<(Rect, String)>,
    pub loading: bool,
}

/// Paint the parsed `blocks` into `viewport`, scrolled by `scroll` px, clipping
/// to the viewport. `block_remote` gates remote (non-`data:`) images. Returns
/// the layout metrics the reader needs to clamp scrolling and hit-test links.
#[allow(clippy::too_many_arguments)]
pub fn draw_body(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    images: &ImageStore,
    blocks: &[Block],
    viewport: Rect,
    scroll: f64,
    block_remote: bool,
) -> Rendered {
    let c = &theme.tokens.colors;
    let width = (viewport.width() - 4.0).max(40.0);
    let x = viewport.x0;
    let mut out = Rendered::default();

    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &viewport);
    let mut y = viewport.y0 - scroll;
    for block in blocks {
        let top = y;
        match block {
            Block::Heading(level, spans) => {
                let size = match level {
                    1 => 22.0,
                    2 => 19.0,
                    3 => 16.5,
                    _ => 15.0,
                };
                let (layout, links) = shape(tr, spans, size, true, width, theme);
                let h = layout.height() as f64;
                tr.draw_rich(scene, &layout, t * Affine::translate((x, y + 6.0)), c.text_primary);
                collect_links(&mut out.links, &layout, &links, x, y + 6.0);
                y += h + 14.0;
            }
            Block::Paragraph(spans) => {
                let (layout, links) = shape(tr, spans, BODY_SIZE, false, width, theme);
                let h = layout.height() as f64;
                tr.draw_rich(scene, &layout, t * Affine::translate((x, y)), c.neutral[8]);
                collect_links(&mut out.links, &layout, &links, x, y);
                y += h + 12.0;
            }
            Block::ListItem(marker, spans) => {
                let indent = 22.0;
                let label = match marker {
                    Some(n) => format!("{n}."),
                    None => "•".to_string(),
                };
                tr.draw_text(scene, &label, BODY_SIZE, c.text_dim, t * Affine::translate((x + 4.0, y)), None);
                let (layout, links) = shape(tr, spans, BODY_SIZE, false, width - indent, theme);
                let h = layout.height() as f64;
                tr.draw_rich(scene, &layout, t * Affine::translate((x + indent, y)), c.neutral[8]);
                collect_links(&mut out.links, &layout, &links, x + indent, y);
                y += h + 6.0;
            }
            Block::Quote(spans) => {
                let indent = 16.0;
                let (layout, links) = shape(tr, spans, BODY_SIZE, false, width - indent, theme);
                let h = layout.height() as f64;
                let bar = Rect::new(x, y, x + 3.0, y + h + 2.0);
                scene.fill(Fill::NonZero, t, c.accent[5], None, &bar);
                tr.draw_rich(scene, &layout, t * Affine::translate((x + indent, y)), c.text_dim);
                collect_links(&mut out.links, &layout, &links, x + indent, y);
                y += h + 16.0;
            }
            Block::Image { src, alt, w, h } => {
                let consumed = draw_image(
                    scene, t, tr, theme, images, src, alt, *w, *h, x, y, width, block_remote,
                    &mut out.loading,
                );
                y += consumed + 12.0;
            }
            Block::Rule => {
                scene.fill(Fill::NonZero, t, c.border_standard, None, &Rect::new(x, y + 6.0, x + width, y + 7.0));
                y += 18.0;
            }
        }
        let _ = top;
    }
    scene.pop_layer();
    out.content_height = (y + scroll) - viewport.y0;
    out
}

/// Shape a block's inline spans, returning the layout plus each link's byte
/// range (used to hit-test after paint). Links render in the accent colour and
/// underlined.
fn shape(
    tr: &mut TextRenderer,
    spans: &[Span],
    size: f32,
    bold: bool,
    width: f64,
    theme: &Theme,
) -> (RichTextLayout, Vec<(usize, usize, String)>) {
    let accent = rgb(theme.tokens.colors.accent[5]);
    let mut rich: Vec<RichSpan> = Vec::new();
    let mut underlines: Vec<(usize, usize)> = Vec::new();
    let mut links: Vec<(usize, usize, String)> = Vec::new();
    let mut cursor = 0usize;
    for s in spans {
        if s.text.is_empty() {
            continue;
        }
        let start = cursor;
        let end = cursor + s.text.len();
        cursor = end;
        let mut rs = RichSpan::new(s.text.clone()).bold(s.bold).italic(s.italic);
        if let Some(href) = &s.link {
            rs = rs.color(Some(accent));
            underlines.push((start, end));
            links.push((start, end, href.clone()));
        }
        rich.push(rs);
    }
    if rich.is_empty() {
        rich.push(RichSpan::new(String::new()));
    }
    let layout = RichTextLayout::shape(
        &mut tr.font_system,
        &rich,
        bold,
        size,
        width as f32,
        TextAlign::Left,
        FontFamily::Sans,
        underlines,
        vec![],
        1.0,
    );
    (layout, links)
}

/// Turn a block's link byte-ranges into absolute hit-rects at `(x, y)`.
fn collect_links(
    out: &mut Vec<(Rect, String)>,
    layout: &RichTextLayout,
    links: &[(usize, usize, String)],
    x: f64,
    y: f64,
) {
    for (s, e, href) in links {
        for (rx0, ry0, rx1, ry1) in layout.selection_rects(*s, *e) {
            out.push((
                Rect::new(x + rx0 as f64, y + ry0 as f64, x + rx1 as f64, y + ry1 as f64),
                href.clone(),
            ));
        }
    }
}

/// Draw one image block (or its placeholder) and return the height it consumed.
/// `loading` is set when an allowed image has been requested but isn't decoded
/// yet, so the reader keeps pumping frames until it appears.
#[allow(clippy::too_many_arguments)]
fn draw_image(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    images: &ImageStore,
    src: &str,
    alt: &str,
    decl_w: Option<u32>,
    decl_h: Option<u32>,
    x: f64,
    y: f64,
    width: f64,
    block_remote: bool,
    loading: &mut bool,
) -> f64 {
    let c = &theme.tokens.colors;
    let is_data = src.to_ascii_lowercase().starts_with("data:");
    let allowed = is_data || !block_remote;

    if allowed {
        if let Some(img) = images.get(src) {
            let iw = img.width.max(1) as f64;
            let ih = img.height.max(1) as f64;
            // Respect a declared width, never upscale past natural size, and
            // never exceed the content column.
            let cap = decl_w.map(|v| v as f64).unwrap_or(iw).min(width);
            let dw = cap.min(iw).max(1.0);
            let dh = dw * ih / iw;
            let rect = Rect::new(x, y, x + dw, y + dh);
            let scale = dw / iw;
            let brush = ImageBrush::new(img);
            scene.fill(
                Fill::NonZero,
                t,
                &brush,
                Some(Affine::translate((x, y)) * Affine::scale(scale)),
                &rect,
            );
            return dh;
        }
        // Requested, not yet decoded → a neutral loading tile.
        *loading = true;
        return placeholder(scene, t, tr, theme, x, y, width, decl_h, Icon::Image, if alt.is_empty() { "Loading image…" } else { alt }, c.text_dim);
    }
    // Remote image, still blocked: a labelled placeholder (never a network hit).
    placeholder(
        scene, t, tr, theme, x, y, width, decl_h, Icon::Ban,
        if alt.is_empty() { "Remote image blocked" } else { alt },
        c.semantic.warning,
    )
}

/// A framed placeholder box for an image that isn't painted this frame.
#[allow(clippy::too_many_arguments)]
fn placeholder(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    x: f64,
    y: f64,
    width: f64,
    decl_h: Option<u32>,
    icon: Icon,
    label: &str,
    icon_col: vello::peniko::Color,
) -> f64 {
    let c = &theme.tokens.colors;
    let h = decl_h.map(|v| (v as f64).clamp(48.0, 220.0)).unwrap_or(120.0);
    let w = width.min(360.0);
    let box_r = Rect::new(x, y, x + w, y + h);
    // `border_standard` reads in both light and dark, unlike the white-tinted
    // overlay tokens which vanish on a light surface.
    scene.fill(Fill::NonZero, t, c.surface_white_5, None, &vello::kurbo::RoundedRect::from_rect(box_r, 8.0));
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        t,
        c.border_standard,
        None,
        &vello::kurbo::RoundedRect::from_rect(box_r, 8.0),
    );
    let icon_box = Rect::new(box_r.x0 + 12.0, box_r.y0 + h / 2.0 - 9.0, box_r.x0 + 30.0, box_r.y0 + h / 2.0 + 9.0);
    icon.draw(scene, t, icon_box, icon_col);
    tr.draw_text(scene, label, 12.0, c.text_dim, t * Affine::translate((box_r.x0 + 38.0, box_r.y0 + h / 2.0 - 8.0)), Some((w - 46.0) as f32));
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headings_paragraphs_lists_and_quotes() {
        let html = "<h1>Title</h1><p>Hello <strong>world</strong>.</p>\
            <ul><li>one</li><li>two</li></ul><blockquote>quote</blockquote>";
        let blocks = parse(html);
        assert!(matches!(blocks[0], Block::Heading(1, _)), "h1 → heading");
        // The paragraph keeps its bold run.
        match &blocks[1] {
            Block::Paragraph(spans) => {
                assert!(spans.iter().any(|s| s.bold && s.text.contains("world")), "strong is bold");
                assert!(spans.iter().any(|s| s.text.contains("Hello")));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
        assert!(matches!(blocks[2], Block::ListItem(None, _)), "ul item is a bullet");
        assert!(matches!(blocks[3], Block::ListItem(None, _)));
        assert!(matches!(blocks.last(), Some(Block::Quote(_))));
    }

    #[test]
    fn html_only_body_yields_readable_text_not_an_empty_block() {
        // A realistic HTML-only body (no plain-text part) must parse into real,
        // non-empty text blocks — the fix for "it doesn't feel loaded".
        let html = "<html><body><h2>Weekly</h2><p>The shell landed &amp; shipped.</p></body></html>";
        let blocks = parse(html);
        let text: String = blocks
            .iter()
            .flat_map(|b| match b {
                Block::Heading(_, s) | Block::Paragraph(s) => s.clone(),
                _ => vec![],
            })
            .map(|s| s.text)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("Weekly"));
        assert!(text.contains("The shell landed & shipped."), "entities decode: {text}");
    }

    #[test]
    fn scripts_and_styles_are_dropped_entirely() {
        let blocks = parse("<script>steal()</script><style>x{}</style><p>safe</p>");
        assert_eq!(blocks.len(), 1, "only the paragraph survives");
        match &blocks[0] {
            Block::Paragraph(s) => assert_eq!(s[0].text, "safe"),
            other => panic!("unexpected {other:?}"),
        }
        // The script text never leaks into any block.
        assert!(!format!("{blocks:?}").contains("steal"));
    }

    #[test]
    fn inline_runs_keep_word_spacing_across_tags() {
        // The space around <strong> must survive so words don't merge.
        let blocks = parse("<p>The <strong>scrollable-strip shell</strong> landed today.</p>");
        let Block::Paragraph(spans) = &blocks[0] else { panic!() };
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "The scrollable-strip shell landed today.");
    }

    #[test]
    fn only_safe_link_schemes_survive() {
        let blocks = parse(
            "<p><a href=\"https://ok.example/x\">good</a> <a href=\"javascript:evil()\">bad</a></p>",
        );
        let Block::Paragraph(spans) = &blocks[0] else { panic!() };
        let good = spans.iter().find(|s| s.text == "good").unwrap();
        assert_eq!(good.link.as_deref(), Some("https://ok.example/x"));
        let bad = spans.iter().find(|s| s.text == "bad").unwrap();
        assert_eq!(bad.link, None, "javascript: link is stripped to inert text");
    }

    #[test]
    fn tracking_pixels_are_dropped_but_real_images_kept() {
        let blocks = parse(
            "<img src=\"https://track.example/o.gif\" width=\"1\" height=\"1\">\
             <img src=\"https://cdn.example/banner.png\" width=\"320\" height=\"80\">",
        );
        let imgs: Vec<_> = blocks.iter().filter(|b| matches!(b, Block::Image { .. })).collect();
        assert_eq!(imgs.len(), 1, "the 1×1 tracking pixel is dropped");
        match imgs[0] {
            Block::Image { src, w, h, .. } => {
                assert!(src.contains("banner.png"));
                assert_eq!((*w, *h), (Some(320), Some(80)));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn data_uri_decodes_base64_and_non_data_returns_none() {
        // "Zm9v" == "foo".
        assert_eq!(decode_data_uri("data:text/plain;base64,Zm9v"), Some(b"foo".to_vec()));
        assert_eq!(decode_data_uri("https://x.example/a.png"), None);
        // A tiny valid PNG header round-trips through base64.
        let png = decode_data_uri("data:image/png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(&png[..4], &[0x89, 0x50, 0x4e, 0x47]);
    }

    // ---- rendering (real fetch → decode → place through ImageStore) --------

    /// A tiny but genuinely valid 4×4 PNG (blue), base64-encoded — decoded by
    /// `ui::ImageStore`'s real `image`-crate path with no network.
    const TINY_PNG_B64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAIAAAAmkwkpAAAAEElEQVR42mPQqLgDRwzEcQBekhfBAVlN3gAAAABJRU5ErkJggg==";

    fn png_bytes() -> Vec<u8> {
        base64_decode(TINY_PNG_B64).unwrap()
    }

    fn render_once(images: &ImageStore, blocks: &[Block], block_remote: bool) -> Rendered {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        draw_body(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            images,
            blocks,
            Rect::new(0.0, 0.0, 600.0, 400.0),
            0.0,
            block_remote,
        )
    }

    #[test]
    fn remote_image_loads_only_when_allowed_and_then_places() {
        use std::sync::Arc;
        use std::time::Duration;
        use ui::{Fetcher, ImageStore};

        let bytes = png_bytes();
        let fetch: Fetcher = Arc::new(move |url: &str| {
            // data: decodes locally; the known remote URL returns the PNG.
            decode_data_uri(url).or_else(|| url.contains("cdn.example").then(|| bytes.clone()))
        });
        let images = ImageStore::new(fetch);
        let url = "https://cdn.example/banner.png";
        let blocks = vec![Block::Image {
            src: url.into(),
            alt: "banner".into(),
            w: Some(320),
            h: Some(80),
        }];

        // Blocked: the remote image is never requested (no network hit).
        render_once(&images, &blocks, true);
        assert!(!images.is_known(url), "a blocked remote image is never fetched");

        // Opted in: it is requested, decodes, and a subsequent draw places it
        // (no lingering loading tile).
        let first = render_once(&images, &blocks, false);
        assert!(images.is_known(url), "opting in requests the remote image");
        assert!(first.loading, "the first frame shows a loading placeholder");
        images.block_until_idle(Duration::from_secs(5));
        assert!(images.get(url).is_some(), "the remote image decoded + cached");
        let placed = render_once(&images, &blocks, false);
        assert!(!placed.loading, "once decoded the real image is placed, not a tile");
        assert!(placed.content_height > 0.0, "the placed image has real height");
    }

    #[test]
    fn data_image_places_even_when_remote_is_blocked() {
        use std::sync::Arc;
        use std::time::Duration;
        use ui::{Fetcher, ImageStore};

        // An inline data: image the fetcher decodes locally (no network).
        let src = format!("data:image/png;base64,{TINY_PNG_B64}");
        let fetch: Fetcher = Arc::new(|url: &str| decode_data_uri(url));
        let images = ImageStore::new(fetch);
        let blocks = vec![Block::Image { src: src.clone(), alt: String::new(), w: Some(4), h: Some(4) }];

        // Even with remote blocked, the inline image is requested + placed.
        render_once(&images, &blocks, true);
        assert!(images.is_known(&src), "an inline data: image is always allowed");
        images.block_until_idle(Duration::from_secs(5));
        assert!(images.get(&src).is_some(), "the inline image decoded + cached");
        let placed = render_once(&images, &blocks, true);
        assert!(!placed.loading, "the inline image is placed even while remote is blocked");
    }
}
