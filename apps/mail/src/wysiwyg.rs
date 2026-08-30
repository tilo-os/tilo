//! A small, self-contained **WYSIWYG rich-text editor** for HTML mail bodies.
//!
//! Its attributed paragraphs are shaped, wrapped, painted and hit-tested by
//! the toolkit's [`RichTextLayout`], so it shares ligature, BiDi, Unicode-break
//! and caret geometry with the other editors. The toolkit's existing
//! `BlockEditor` is Markdown-only, single-line-per-block, and has no
//! underline / block-quote / `href` link, so an HTML-mail editor needs its own
//! component; Mail therefore retains the HTML document model and serialization.
//!
//! ## Model
//! A document is a list of [`Block`]s. Each block has a [`BlockKind`]
//! (paragraph, H1–H3, bullet / numbered list item, block-quote) and a run of
//! text carrying per-character inline [`Style`] flags (bold / italic /
//! underline) plus an optional link target. The caret is a `(block, character)`
//! pair; a selection is an ordered caret pair. This is deliberately a flat,
//! easy-to-serialise model rather than a tree.
//!
//! ## Serialisation
//! * [`Wysiwyg::to_html`] emits clean, semantic HTML
//!   (`<h1>`/`<p>`/`<ul><li>`/`<ol><li>`/`<blockquote>`, with `<b>`/`<i>`/`<u>`/
//!   `<a href>` inline) suitable for the `text/html` MIME part.
//! * [`Wysiwyg::to_plain`] flattens to plain text (one block per line, list
//!   markers preserved) for the `text/plain` alternative / raw-text mode.
//! * [`Wysiwyg::from_plain`] seeds the document from plain text (used when the
//!   user switches Raw → HTML, or to import a reply quote).

use std::fmt::Write as _;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::color_compat::ColorCompat;
use ui::richtext::{RichSpan, RichTextLayout, TextAlign};
use ui::text_render::{TextRenderer, TextWeight};
use ui::text_selection::{next_char_boundary, prev_char_boundary};
use ui::theme::Theme;
use ui::tokens::TextRole;

/// Inline character styling. Stored per byte run; toggled over the selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// One styled character. Keeping style per-char (rather than per-span) makes
/// toggling a mark over an arbitrary selection trivial and correct; runs are
/// re-coalesced at draw / serialise time.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Glyph {
    ch: char,
    style: Style,
    /// Index into [`Block::links`] if this character is part of a link.
    link: Option<usize>,
}

/// Block-level kind. Lists carry the marker only; consecutive list blocks of
/// the same kind are wrapped in a single `<ul>`/`<ol>` at serialise time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    Bullet,
    Numbered,
    Quote,
}

impl BlockKind {
    fn html_tag(self) -> &'static str {
        match self {
            BlockKind::Paragraph => "p",
            BlockKind::Heading(1) => "h1",
            BlockKind::Heading(2) => "h2",
            BlockKind::Heading(_) => "h3",
            BlockKind::Bullet | BlockKind::Numbered => "li",
            BlockKind::Quote => "blockquote",
        }
    }
}

#[derive(Clone, Debug)]
struct Block {
    kind: BlockKind,
    glyphs: Vec<Glyph>,
    /// Link targets referenced by `Glyph::link`.
    links: Vec<String>,
}

impl Block {
    fn paragraph() -> Self {
        Self {
            kind: BlockKind::Paragraph,
            glyphs: Vec::new(),
            links: Vec::new(),
        }
    }

    fn text(&self) -> String {
        self.glyphs.iter().map(|g| g.ch).collect()
    }
}

/// A `(block index, glyph index)` caret position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Caret {
    block: usize,
    glyph: usize,
}

const LINE_GAP: f64 = 6.0;
const BLOCK_GAP: f64 = 10.0;
const PAD_X: f64 = 12.0;
const PAD_Y: f64 = 10.0;
const LIST_INDENT: f64 = 22.0;
const QUOTE_INDENT: f64 = 16.0;

/// The editor. Owns its document, caret, selection, focus, scroll, and the
/// laid-out per-block rects used for hit-testing.
pub struct Wysiwyg {
    blocks: Vec<Block>,
    caret: Caret,
    /// Selection anchor; `Some` while a selection is active.
    anchor: Option<Caret>,
    focused: bool,
    rect: Rect,
    /// "Next typed character takes these marks" — set when the user toggles a
    /// mark with an empty selection (so they can turn on bold then type).
    pending: Style,
    /// Per-block laid-out content rects (in screen coords), recomputed each
    /// `layout`. Used for click hit-testing and caret placement.
    block_rects: Vec<Rect>,
    /// Authoritative attributed layouts from the latest draw. Pointer hit
    /// testing reads these instead of maintaining a second approximate row
    /// model, so ligatures, BiDi and soft wrapping agree with the ink.
    block_layouts: Vec<RichTextLayout>,
    scroll_y: f64,
    dirty: bool,
}

impl Default for Wysiwyg {
    fn default() -> Self {
        Self::new()
    }
}

impl Wysiwyg {
    pub fn new() -> Self {
        Self {
            blocks: vec![Block::paragraph()],
            caret: Caret { block: 0, glyph: 0 },
            anchor: None,
            focused: false,
            rect: Rect::ZERO,
            pending: Style::default(),
            block_rects: Vec::new(),
            block_layouts: Vec::new(),
            scroll_y: 0.0,
            dirty: true,
        }
    }

    /// Seed the document from plain text. Blank lines separate paragraphs;
    /// lines beginning with `> ` become block-quotes (so a reply quote imports
    /// sensibly). Caret parks at the end.
    pub fn from_plain(text: &str) -> Self {
        let mut ed = Self::new();
        ed.set_plain(text);
        ed
    }

    /// Replace the whole document with plain text (used by Raw → HTML switch).
    pub fn set_plain(&mut self, text: &str) {
        self.blocks.clear();
        for line in text.split('\n') {
            let (kind, body) = if let Some(rest) = line.strip_prefix("> ") {
                (BlockKind::Quote, rest)
            } else {
                (BlockKind::Paragraph, line)
            };
            let mut b = Block {
                kind,
                glyphs: Vec::new(),
                links: Vec::new(),
            };
            for ch in body.chars() {
                b.glyphs.push(Glyph {
                    ch,
                    style: Style::default(),
                    link: None,
                });
            }
            self.blocks.push(b);
        }
        if self.blocks.is_empty() {
            self.blocks.push(Block::paragraph());
        }
        self.caret = self.end_caret();
        self.anchor = None;
        self.dirty = true;
    }

    /// A caret parked at the very end of the last block — used after
    /// seeding/appending content. Falls back to `(0, 0)` if `blocks` is ever
    /// unexpectedly empty (the invariant is that it never is).
    fn end_caret(&self) -> Caret {
        match self.blocks.len() {
            0 => Caret { block: 0, glyph: 0 },
            n => Caret {
                block: n - 1,
                glyph: self.blocks.get(n - 1).map_or(0, |b| b.glyphs.len()),
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|b| b.glyphs.is_empty())
    }

    /// Append a block built from styled segments. Each segment is `(text,
    /// style)`. Used to seed representative formatted content (and a handy
    /// authoring helper for any host that wants to insert pre-styled blocks).
    pub fn push_styled_block(&mut self, kind: BlockKind, segments: &[(&str, Style)]) {
        // Replace the initial empty paragraph the constructor leaves in place.
        if self.blocks.len() == 1 && self.blocks.first().is_some_and(|b| b.glyphs.is_empty()) {
            self.blocks.clear();
        }
        let mut b = Block {
            kind,
            glyphs: Vec::new(),
            links: Vec::new(),
        };
        for (text, style) in segments {
            for ch in text.chars() {
                b.glyphs.push(Glyph {
                    ch,
                    style: *style,
                    link: None,
                });
            }
        }
        self.blocks.push(b);
        self.caret = self.end_caret();
        self.dirty = true;
    }

    pub fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn get_rect(&self) -> Rect {
        self.rect
    }

    // ---- serialisation ------------------------------------------------

    /// Emit semantic HTML for the `text/html` MIME part. Adjacent list items of
    /// the same kind are wrapped in one `<ul>` / `<ol>`.
    pub fn to_html(&self) -> String {
        let mut out = String::new();
        let mut i = 0;
        while let Some(b) = self.blocks.get(i) {
            let kind = b.kind;
            match kind {
                BlockKind::Bullet | BlockKind::Numbered => {
                    let ordered = matches!(kind, BlockKind::Numbered);
                    out.push_str(if ordered { "<ol>" } else { "<ul>" });
                    while let Some(item) = self.blocks.get(i).filter(|blk| blk.kind == kind) {
                        out.push_str("<li>");
                        out.push_str(&inline_html(item));
                        out.push_str("</li>");
                        i += 1;
                    }
                    out.push_str(if ordered { "</ol>" } else { "</ul>" });
                    continue;
                }
                _ => {
                    let tag = kind.html_tag();
                    out.push('<');
                    out.push_str(tag);
                    out.push('>');
                    out.push_str(&inline_html(b));
                    out.push_str("</");
                    out.push_str(tag);
                    out.push('>');
                }
            }
            i += 1;
        }
        out
    }

    /// Flatten to plain text for the `text/plain` part / raw-text mode.
    pub fn to_plain(&self) -> String {
        let mut out = String::new();
        let mut num = 1;
        for (i, b) in self.blocks.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            match b.kind {
                BlockKind::Bullet => out.push_str("• "),
                BlockKind::Numbered => {
                    let _ = write!(out, "{num}. ");
                    num += 1;
                }
                BlockKind::Quote => out.push_str("> "),
                _ => {}
            }
            if !matches!(b.kind, BlockKind::Numbered) {
                num = 1;
            }
            out.push_str(&b.text());
        }
        out
    }

    // ---- formatting commands (driven by the compose toolbar) ----------

    /// Toggle a bold/italic/underline mark over the current selection, or arm
    /// it for the next typed character when the selection is empty.
    pub fn toggle_inline(&mut self, which: InlineMark) {
        let Some((a, b)) = self.ordered_selection() else {
            // No selection: toggle the pending style for the next character.
            match which {
                InlineMark::Bold => self.pending.bold = !self.pending.bold,
                InlineMark::Italic => self.pending.italic = !self.pending.italic,
                InlineMark::Underline => self.pending.underline = !self.pending.underline,
            }
            self.dirty = true;
            return;
        };
        // Decide add vs remove: if every selected char already has the mark,
        // remove it; otherwise add it.
        let all_set = self.selection_glyphs(a, b).all(|g| match which {
            InlineMark::Bold => g.style.bold,
            InlineMark::Italic => g.style.italic,
            InlineMark::Underline => g.style.underline,
        });
        self.for_each_selected_glyph(a, b, |g| match which {
            InlineMark::Bold => g.style.bold = !all_set,
            InlineMark::Italic => g.style.italic = !all_set,
            InlineMark::Underline => g.style.underline = !all_set,
        });
        self.dirty = true;
    }

    /// Set the block kind of every block touched by the selection (or the caret
    /// block). Toggling a kind that's already set reverts to Paragraph.
    pub fn set_block_kind(&mut self, kind: BlockKind) {
        let (a, b) = self.ordered_selection().unwrap_or((self.caret, self.caret));
        let already =
            (a.block..=b.block).all(|i| self.blocks.get(i).is_some_and(|blk| blk.kind == kind));
        if let Some(range) = self.blocks.get_mut(a.block..=b.block) {
            for blk in range {
                blk.kind = if already { BlockKind::Paragraph } else { kind };
            }
        }
        self.dirty = true;
    }

    /// Wrap the current selection in a link to `href`. With an empty selection
    /// this inserts the href text itself as the link label.
    pub fn set_link(&mut self, href: &str) {
        if href.trim().is_empty() {
            return;
        }
        if self.ordered_selection().is_none() {
            // Insert the URL as its own linked text.
            for ch in href.chars() {
                self.insert_char(ch);
            }
            // Re-select what we just inserted and link it.
            let end = self.caret;
            let start = Caret {
                block: end.block,
                glyph: end.glyph - href.chars().count(),
            };
            self.anchor = Some(start);
            self.caret = end;
        }
        let Some((a, b)) = self.ordered_selection() else {
            // Unreachable in practice: the branch above just turned an empty
            // selection into a real one, or one already existed — but bail
            // rather than panic if that invariant is ever violated.
            return;
        };
        // One link entry per block touched (links don't span blocks here).
        for bi in a.block..=b.block {
            let (s, e) = self.block_selection_bounds(bi, a, b);
            let Some(block) = self.blocks.get_mut(bi) else {
                continue;
            };
            let li = block.links.len();
            block.links.push(href.to_string());
            if let Some(glyphs) = block.glyphs.get_mut(s..e) {
                for g in glyphs {
                    g.link = Some(li);
                    g.style.underline = true;
                }
            }
        }
        self.dirty = true;
    }

    // ---- internal selection helpers -----------------------------------

    fn ordered_selection(&self) -> Option<(Caret, Caret)> {
        let anchor = self.anchor?;
        if anchor == self.caret {
            return None;
        }
        Some(if anchor <= self.caret {
            (anchor, self.caret)
        } else {
            (self.caret, anchor)
        })
    }

    /// The `[start, end)` glyph range of block `bi` that lies inside selection
    /// `(a, b)`.
    fn block_selection_bounds(&self, bi: usize, a: Caret, b: Caret) -> (usize, usize) {
        let len = self.blocks.get(bi).map_or(0, |blk| blk.glyphs.len());
        let s = if bi == a.block { a.glyph } else { 0 };
        let e = if bi == b.block { b.glyph } else { len };
        (s.min(len), e.min(len))
    }

    fn selection_glyphs(&self, a: Caret, b: Caret) -> impl Iterator<Item = &Glyph> + '_ {
        (a.block..=b.block).flat_map(move |bi| {
            let (s, e) = self.block_selection_bounds(bi, a, b);
            self.blocks
                .get(bi)
                .and_then(|blk| blk.glyphs.get(s..e))
                .into_iter()
                .flatten()
        })
    }

    fn for_each_selected_glyph(&mut self, a: Caret, b: Caret, mut apply: impl FnMut(&mut Glyph)) {
        for bi in a.block..=b.block {
            let (start, end) = self.block_selection_bounds(bi, a, b);
            if let Some(glyphs) = self
                .blocks
                .get_mut(bi)
                .and_then(|blk| blk.glyphs.get_mut(start..end))
            {
                for g in glyphs {
                    apply(g);
                }
            }
        }
    }

    // ---- text editing -------------------------------------------------

    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.ordered_selection() else {
            return false;
        };
        if a.block == b.block {
            if let Some(blk) = self.blocks.get_mut(a.block) {
                blk.glyphs.drain(a.glyph..b.glyph);
            }
        } else {
            // Keep head of first block, tail of last block, drop the middle.
            let tail: Vec<Glyph> = self
                .blocks
                .get_mut(b.block)
                .map(|blk| blk.glyphs.split_off(b.glyph))
                .unwrap_or_default();
            if let Some(blk) = self.blocks.get_mut(a.block) {
                blk.glyphs.truncate(a.glyph);
                blk.glyphs.extend(tail);
            }
            self.blocks.drain(a.block + 1..=b.block);
        }
        self.caret = a;
        self.anchor = None;
        self.dirty = true;
        true
    }

    fn insert_char(&mut self, ch: char) {
        self.delete_selection();
        let style = self.pending;
        let Some(b) = self.blocks.get_mut(self.caret.block) else {
            return;
        };
        b.glyphs.insert(
            self.caret.glyph,
            Glyph {
                ch,
                style,
                link: None,
            },
        );
        self.caret.glyph += 1;
        self.dirty = true;
    }

    fn split_block(&mut self) {
        self.delete_selection();
        let Some(cur) = self.blocks.get_mut(self.caret.block) else {
            return;
        };
        let tail = cur.glyphs.split_off(self.caret.glyph);
        let cur_kind = cur.kind;
        // Pressing Enter in a list continues the list; elsewhere a new
        // paragraph. An Enter on an empty list item exits the list.
        let new_kind = match cur_kind {
            BlockKind::Bullet | BlockKind::Numbered if cur.glyphs.is_empty() => {
                cur.kind = BlockKind::Paragraph;
                self.dirty = true;
                return;
            }
            BlockKind::Bullet | BlockKind::Numbered => cur_kind,
            _ => BlockKind::Paragraph,
        };
        let new = Block {
            kind: new_kind,
            glyphs: tail,
            links: Vec::new(),
        };
        self.blocks.insert(self.caret.block + 1, new);
        self.caret = Caret {
            block: self.caret.block + 1,
            glyph: 0,
        };
        self.pending = Style::default();
        self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.caret.glyph > 0 {
            if let Some(block) = self.blocks.get(self.caret.block) {
                let byte = glyph_byte(&block.glyphs, self.caret.glyph);
                let previous = prev_char_boundary(&block.text(), byte);
                let previous_glyph = glyph_index_at_byte(&block.glyphs, previous);
                if let Some(block) = self.blocks.get_mut(self.caret.block) {
                    block.glyphs.drain(previous_glyph..self.caret.glyph);
                }
                self.caret.glyph = previous_glyph;
            }
        } else if self.caret.block > 0 {
            // Merge with previous block.
            let prev = self.caret.block - 1;
            let prev_len = self.blocks.get(prev).map_or(0, |b| b.glyphs.len());
            let moved = self
                .blocks
                .get_mut(self.caret.block)
                .map(|blk| std::mem::take(&mut blk.glyphs))
                .unwrap_or_default();
            if let Some(blk) = self.blocks.get_mut(prev) {
                blk.glyphs.extend(moved);
            }
            self.blocks.remove(self.caret.block);
            self.caret = Caret {
                block: prev,
                glyph: prev_len,
            };
        }
        self.dirty = true;
    }

    fn move_caret(&mut self, dx: i32, extend: bool) {
        if extend && self.anchor.is_none() {
            self.anchor = Some(self.caret);
        } else if !extend {
            self.anchor = None;
        }
        if dx < 0 {
            if self.caret.glyph > 0 {
                let block = &self.blocks[self.caret.block];
                let byte = glyph_byte(&block.glyphs, self.caret.glyph);
                self.caret.glyph =
                    glyph_index_at_byte(&block.glyphs, prev_char_boundary(&block.text(), byte));
            } else if self.caret.block > 0 {
                self.caret.block -= 1;
                self.caret.glyph = self
                    .blocks
                    .get(self.caret.block)
                    .map_or(0, |b| b.glyphs.len());
            }
        } else if self
            .blocks
            .get(self.caret.block)
            .is_some_and(|b| self.caret.glyph < b.glyphs.len())
        {
            if let Some(block) = self.blocks.get(self.caret.block) {
                let byte = glyph_byte(&block.glyphs, self.caret.glyph);
                self.caret.glyph =
                    glyph_index_at_byte(&block.glyphs, next_char_boundary(&block.text(), byte));
            }
        } else if self.caret.block + 1 < self.blocks.len() {
            self.caret.block += 1;
            self.caret.glyph = 0;
        }
        self.pending = Style::default();
        self.dirty = true;
    }

    // ---- layout, draw, events -----------------------------------------

    pub fn apply_layout(&mut self, rect: Rect) {
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    /// Current marks at the caret (selection-wide if any) — drives the
    /// toolbar's active-button highlighting.
    pub fn active_marks(&self) -> Style {
        if let Some((a, b)) = self.ordered_selection() {
            let mut all = Style {
                bold: true,
                italic: true,
                underline: true,
            };
            let mut any = false;
            for g in self.selection_glyphs(a, b) {
                any = true;
                all.bold &= g.style.bold;
                all.italic &= g.style.italic;
                all.underline &= g.style.underline;
            }
            if any {
                return all;
            }
        }
        self.pending
    }

    /// Block kind at the caret — drives toolbar block-button highlighting.
    pub fn active_block_kind(&self) -> BlockKind {
        self.blocks
            .get(self.caret.block)
            .map_or(BlockKind::Paragraph, |b| b.kind)
    }

    pub fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        self.block_rects.clear();
        self.block_layouts.clear();
        let content_w = (self.rect.width() - 2.0 * PAD_X).max(40.0);
        let mut y = self.rect.y0 + PAD_Y - self.scroll_y;
        let mut list_num = 1;

        if self.is_empty() && !self.focused {
            let style = theme.tokens.typography.style(TextRole::Body);
            tr.draw_style(
                scene,
                "Write your message…",
                c.text_dim,
                t * Affine::translate((self.rect.x0 + PAD_X, self.rect.y0 + PAD_Y)),
                None,
                &style,
            );
        }

        let selection = self.ordered_selection();
        let link_rgba = c.accent[4].to_rgba8();
        let link_rgb = [link_rgba.r, link_rgba.g, link_rgba.b];
        for (bi, b) in self.blocks.iter().enumerate() {
            let style = match b.kind {
                BlockKind::Heading(1) => theme.tokens.typography.style(TextRole::Heading),
                BlockKind::Heading(2) => theme
                    .tokens
                    .typography
                    .style(TextRole::Title)
                    .with_weight(TextWeight::Bold),
                BlockKind::Heading(_) => theme
                    .tokens
                    .typography
                    .style(TextRole::Body)
                    .with_weight(TextWeight::Semibold),
                _ => theme.tokens.typography.style(TextRole::Body),
            };
            let indent = match b.kind {
                BlockKind::Bullet | BlockKind::Numbered => LIST_INDENT,
                BlockKind::Quote => QUOTE_INDENT,
                _ => 0.0,
            };
            let bx = self.rect.x0 + PAD_X + indent;
            let available = (content_w - indent).max(1.0) as f32;

            let mut spans = Vec::new();
            let mut underlines = Vec::new();
            let mut byte = 0usize;
            let mut idx = 0usize;
            while idx < b.glyphs.len() {
                let glyph = &b.glyphs[idx];
                let mut end = idx + 1;
                while end < b.glyphs.len()
                    && b.glyphs[end].style == glyph.style
                    && b.glyphs[end].link == glyph.link
                {
                    end += 1;
                }
                let run: String = b.glyphs[idx..end].iter().map(|item| item.ch).collect();
                let run_end = byte + run.len();
                spans.push(
                    RichSpan::new(run)
                        .bold(glyph.style.bold)
                        .italic(glyph.style.italic)
                        .color(glyph.link.map(|_| link_rgb)),
                );
                if glyph.style.underline || glyph.link.is_some() {
                    underlines.push((byte, run_end));
                }
                byte = run_end;
                idx = end;
            }
            if spans.is_empty() {
                spans.push(RichSpan::new(String::new()));
            }
            let layout = RichTextLayout::shape_styled(
                &mut tr.font_system,
                &spans,
                &style,
                Some(available),
                TextAlign::Left,
                underlines,
                Vec::new(),
            );
            let layout_h = f64::from(layout.height());

            // Quote bar.
            if matches!(b.kind, BlockKind::Quote) {
                let bar = Rect::new(
                    self.rect.x0 + PAD_X,
                    y,
                    self.rect.x0 + PAD_X + 3.0,
                    y + layout_h,
                );
                scene.fill(vello::peniko::Fill::NonZero, t, c.accent[5], None, &bar);
            }
            // List marker.
            if matches!(b.kind, BlockKind::Bullet) {
                tr.draw_style(
                    scene,
                    "•",
                    c.text_dim,
                    t * Affine::translate((self.rect.x0 + PAD_X + 4.0, y)),
                    None,
                    &style,
                );
            } else if matches!(b.kind, BlockKind::Numbered) {
                tr.draw_style(
                    scene,
                    &format!("{list_num}."),
                    c.text_dim,
                    t * Affine::translate((self.rect.x0 + PAD_X, y)),
                    None,
                    &style,
                );
                list_num += 1;
            }
            if !matches!(b.kind, BlockKind::Numbered) {
                list_num = 1;
            }

            let origin = t * Affine::translate((bx, y));
            if let Some((sa, sb)) = selection
                && bi >= sa.block
                && bi <= sb.block
            {
                let (s, e) = self.block_selection_bounds(bi, sa, sb);
                let start = glyph_byte(&b.glyphs, s);
                let end = glyph_byte(&b.glyphs, e);
                for (x0, y0, x1, y1) in layout.selection_rects(start, end) {
                    let sel = Rect::new(f64::from(x0), f64::from(y0), f64::from(x1), f64::from(y1));
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        origin,
                        c.accent[5].with_alpha_factor(0.28),
                        None,
                        &sel,
                    );
                }
            }
            tr.draw_rich(scene, &layout, origin, c.text_primary);

            if self.focused && self.caret.block == bi {
                let caret_byte = glyph_byte(&b.glyphs, self.caret.glyph);
                let (cx, top, height) = layout.caret_geometry(caret_byte);
                let caret = Rect::new(
                    f64::from(cx),
                    f64::from(top),
                    f64::from(cx) + theme.caret_width,
                    f64::from(top + height),
                );
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    origin,
                    c.accent[5],
                    None,
                    &caret,
                );
            }

            let brect = Rect::new(bx, y, bx + f64::from(available), y + layout_h);
            self.block_rects.push(brect);
            self.block_layouts.push(layout);
            y += layout_h
                + if matches!(b.kind, BlockKind::Heading(_)) {
                    BLOCK_GAP
                } else {
                    LINE_GAP
                };
        }
    }

    /// Hit-test a click to a caret position and place it (extending selection
    /// when `extend`).
    fn place_caret_at(&mut self, pt: Point, extend: bool) {
        if extend && self.anchor.is_none() {
            self.anchor = Some(self.caret);
        } else if !extend {
            self.anchor = None;
        }
        // Find the block whose vertical band contains pt.y (clamp to nearest).
        let mut best = 0usize;
        let mut best_dy = f64::MAX;
        for (i, r) in self.block_rects.iter().enumerate() {
            let dy = if pt.y < r.y0 {
                r.y0 - pt.y
            } else if pt.y > r.y1 {
                pt.y - r.y1
            } else {
                0.0
            };
            if dy < best_dy {
                best_dy = dy;
                best = i;
            }
        }
        let glyph = self
            .block_layouts
            .get(best)
            .zip(self.block_rects.get(best))
            .zip(self.blocks.get(best))
            .map_or(0, |((layout, rect), block)| {
                let byte = layout.hit(
                    (pt.x - rect.x0).max(0.0) as f32,
                    (pt.y - rect.y0).max(0.0) as f32,
                );
                glyph_index_at_byte(&block.glyphs, byte)
            });
        self.caret = Caret { block: best, glyph };
        self.pending = Style::default();
        self.dirty = true;
    }

    /// Forward a pointer-moved (for hover/drag-select). `pt` is logical coords.
    pub fn on_cursor_moved(&mut self, pt: Point, dragging: bool) {
        if dragging && self.focused {
            self.place_caret_at(pt, true);
        }
    }

    /// Handle a mouse press inside the editor. Returns true if consumed.
    pub fn on_mouse_press(&mut self, pt: Point) -> bool {
        if !self.rect.contains(pt) {
            return false;
        }
        self.focused = true;
        self.place_caret_at(pt, false);
        true
    }

    /// Handle a key event while focused. Returns true if consumed.
    pub fn on_key(&mut self, event: &WindowEvent, shift: bool, ctrl: bool) -> bool {
        if !self.focused {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    text,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        // Keyboard shortcuts for inline marks (Ctrl-B/I/U).
        if ctrl && let Key::Character(ch) = logical_key {
            match ch.to_ascii_lowercase().as_str() {
                "b" => {
                    self.toggle_inline(InlineMark::Bold);
                    return true;
                }
                "i" => {
                    self.toggle_inline(InlineMark::Italic);
                    return true;
                }
                "u" => {
                    self.toggle_inline(InlineMark::Underline);
                    return true;
                }
                "a" => {
                    self.select_all();
                    return true;
                }
                _ => {}
            }
        }
        match logical_key {
            Key::Named(NamedKey::Backspace) => {
                self.backspace();
                true
            }
            Key::Named(NamedKey::Delete) => {
                if self.ordered_selection().is_some() {
                    self.delete_selection();
                } else {
                    self.move_caret(1, true);
                    self.delete_selection();
                }
                true
            }
            Key::Named(NamedKey::Enter) => {
                self.split_block();
                true
            }
            Key::Named(NamedKey::Space) => {
                self.insert_char(' ');
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.move_caret(-1, shift);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.move_caret(1, shift);
                true
            }
            Key::Named(NamedKey::Tab) => false,
            _ => {
                if let Some(txt) = text {
                    let s = txt.as_str();
                    if !s.is_empty() && !s.chars().any(char::is_control) {
                        for ch in s.chars() {
                            self.insert_char(ch);
                        }
                        return true;
                    }
                }
                false
            }
        }
    }

    fn select_all(&mut self) {
        if self.blocks.is_empty() {
            return;
        }
        self.anchor = Some(Caret { block: 0, glyph: 0 });
        self.caret = self.end_caret();
        self.dirty = true;
    }
}

/// Convert the editor's character index to the shared layout's UTF-8 byte
/// space. The model remains character-attributed for HTML serialization, while
/// all geometry uses the toolkit's canonical byte-offset contract.
fn glyph_byte(glyphs: &[Glyph], index: usize) -> usize {
    glyphs
        .iter()
        .take(index.min(glyphs.len()))
        .map(|glyph| glyph.ch.len_utf8())
        .sum()
}

fn glyph_index_at_byte(glyphs: &[Glyph], byte: usize) -> usize {
    let mut at = 0usize;
    for (index, glyph) in glyphs.iter().enumerate() {
        let next = at + glyph.ch.len_utf8();
        if byte < next {
            return index;
        }
        at = next;
    }
    glyphs.len()
}

/// Inline mark kinds the toolbar can toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineMark {
    Bold,
    Italic,
    Underline,
}

/// Render one block's glyph run to inline HTML, coalescing equal-style runs
/// and wrapping links. HTML-special characters are escaped.
fn inline_html(block: &Block) -> String {
    let mut out = String::new();
    let mut idx = 0;
    while let Some(g) = block.glyphs.get(idx) {
        // Group a maximal run with identical style + link.
        let mut end = idx + 1;
        while block
            .glyphs
            .get(end)
            .is_some_and(|next| next.style == g.style && next.link == g.link)
        {
            end += 1;
        }
        let Some(run) = block.glyphs.get(idx..end) else {
            break;
        };
        let text: String = run.iter().map(|x| x.ch).collect();
        let mut frag = escape_html(&text);
        if g.style.bold {
            frag = format!("<b>{frag}</b>");
        }
        if g.style.italic {
            frag = format!("<i>{frag}</i>");
        }
        if g.style.underline {
            frag = format!("<u>{frag}</u>");
        }
        if let Some(li) = g.link
            && let Some(href) = block.links.get(li)
        {
            frag = format!("<a href=\"{}\">{frag}</a>", escape_attr(href));
        }
        out.push_str(&frag);
        idx = end;
    }
    out
}

fn escape_html(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            _ => o.push(ch),
        }
    }
    o
}

fn escape_attr(s: &str) -> String {
    escape_html(s).replace('"', "&quot;")
}

// ---------------------------------------------------------------------------
// PROMOTION NOTE
// ---------------------------------------------------------------------------
// The app-specific model remains here because it owns HTML serialization and
// links. All visual text mechanics live in `RichTextLayout`; a future promotion
// would therefore concern the HTML/CRDT document model, not another shaper.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_round_trips_into_blocks() {
        let ed = Wysiwyg::from_plain("First line\nSecond line");
        assert_eq!(ed.blocks.len(), 2);
        assert_eq!(ed.blocks[0].text(), "First line");
    }

    #[test]
    fn backspace_deletes_one_extended_grapheme() {
        let mut editor = Wysiwyg::new();
        editor.set_focus(true);
        for character in "A👩‍👩‍👧‍👦".chars() {
            editor.insert_char(character);
        }
        editor.backspace();
        assert_eq!(editor.to_plain(), "A");
    }

    #[test]
    fn proportional_mail_body_wraps_with_shared_layout() {
        let mut editor = Wysiwyg::from_plain(
            "A deliberately long proportional paragraph with ligatures affinity office and wrapping.",
        );
        editor.apply_layout(Rect::new(0.0, 0.0, 180.0, 300.0));
        let mut scene = Scene::new();
        let mut renderer = TextRenderer::new();
        let theme = Theme::light();
        editor.draw(&mut scene, Affine::IDENTITY, &mut renderer, &theme);
        let body = theme.tokens.typography.style(TextRole::Body);
        assert!(editor.block_layouts[0].height() > body.line_height_px());
    }

    #[test]
    fn typing_then_bold_serialises_to_html() {
        let mut ed = Wysiwyg::new();
        ed.set_focus(true);
        for ch in "Hello".chars() {
            ed.insert_char(ch);
        }
        // Select "Hello" and bold it.
        ed.select_all();
        ed.toggle_inline(InlineMark::Bold);
        let html = ed.to_html();
        assert!(html.contains("<b>Hello</b>"), "got: {html}");
        assert!(html.starts_with("<p>"));
    }

    #[test]
    fn heading_and_list_serialise() {
        let mut ed = Wysiwyg::new();
        ed.set_focus(true);
        for ch in "Title".chars() {
            ed.insert_char(ch);
        }
        ed.set_block_kind(BlockKind::Heading(1));
        ed.split_block();
        for ch in "Item".chars() {
            ed.insert_char(ch);
        }
        ed.set_block_kind(BlockKind::Bullet);
        let html = ed.to_html();
        assert!(html.contains("<h1>Title</h1>"), "got: {html}");
        assert!(html.contains("<ul><li>Item</li></ul>"), "got: {html}");
    }

    #[test]
    fn adjacent_list_items_share_one_list() {
        let mut ed = Wysiwyg::new();
        ed.set_focus(true);
        for ch in "One".chars() {
            ed.insert_char(ch);
        }
        ed.set_block_kind(BlockKind::Bullet);
        ed.split_block();
        for ch in "Two".chars() {
            ed.insert_char(ch);
        }
        // split_block continues the bullet list
        let html = ed.to_html();
        assert_eq!(html.matches("<ul>").count(), 1, "got: {html}");
        assert_eq!(html.matches("<li>").count(), 2, "got: {html}");
    }

    #[test]
    fn html_escapes_special_chars() {
        let mut ed = Wysiwyg::new();
        ed.set_focus(true);
        for ch in "a < b & c".chars() {
            ed.insert_char(ch);
        }
        let html = ed.to_html();
        assert!(html.contains("a &lt; b &amp; c"), "got: {html}");
    }

    #[test]
    fn plain_export_keeps_list_markers() {
        let mut ed = Wysiwyg::new();
        ed.set_focus(true);
        for ch in "task".chars() {
            ed.insert_char(ch);
        }
        ed.set_block_kind(BlockKind::Bullet);
        assert_eq!(ed.to_plain(), "• task");
    }

    #[test]
    fn link_wraps_selection_in_anchor() {
        let mut ed = Wysiwyg::new();
        ed.set_focus(true);
        for ch in "site".chars() {
            ed.insert_char(ch);
        }
        ed.select_all();
        ed.set_link("https://qos.example");
        let html = ed.to_html();
        assert!(
            html.contains("<a href=\"https://qos.example\">"),
            "got: {html}"
        );
    }
}
