//! Reflowable text layout + pagination.
//!
//! Given a chapter's [`Block`]s, a content box, and the live [`Settings`], this
//! lays the chapter out as one continuous column with real inline styling
//! (bold / italic / mono) and word-level wrapping, then slices that column into
//! fixed-height pages at line boundaries (so a line is never cut in half).
//!
//! Layout is measurement-bound, so it runs inside `draw` (where a
//! [`TextRenderer`] is available) and is cached per [`LayoutKey`]; it only
//! recomputes when the chapter, content width, or a typography setting changes.

use ui::text_render::{FontChoice, TextRenderer, TextWeight};

use crate::model::{Block, FontKind, Settings};

/// Map a reader [`FontKind`] to the toolkit [`FontChoice`] used for shaping.
pub fn font_choice(kind: &FontKind) -> FontChoice {
    match kind {
        FontKind::Default => FontChoice::SansSerif,
        FontKind::Serif => FontChoice::Serif,
        FontKind::Mono => FontChoice::Monospace,
        FontKind::Named(n) => FontChoice::Named(n.clone()),
    }
}

/// A positioned, single-style run within a line. `x` is relative to the line's
/// left edge (which is `content_x + indent`). A run here is one whitespace-
/// separated word; `char_start`/`char_len` locate it within the block's
/// canonical text so text selection can map a point to a character offset.
#[derive(Clone, Debug)]
pub struct LineRun {
    pub x: f32,
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    /// Char offset of this run's first character within the block's canonical
    /// text (see [`LaidBlock::text`]).
    pub char_start: usize,
    /// Number of chars in this run.
    pub char_len: usize,
}

/// One visual line of a block. `y` is absolute within the chapter's flow.
#[derive(Clone, Debug)]
pub struct LaidLine {
    pub y: f32,
    pub runs: Vec<LineRun>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutKind {
    Heading,
    Paragraph,
    Quote,
    List,
    Separator,
}

/// A laid-out block, ready to paint.
#[derive(Clone, Debug)]
pub struct LaidBlock {
    pub index: usize,
    pub kind: LayoutKind,
    /// Absolute top of the block's first line.
    pub top: f32,
    /// Absolute bottom of the block's last line.
    pub bottom: f32,
    pub size: f32,
    pub line_height: f32,
    /// Left inset for text + decorations, relative to content_x.
    pub indent: f32,
    pub lines: Vec<LaidLine>,
    /// Bullet/number glyph for list items.
    pub bullet: Option<String>,
    /// The block's canonical selectable text: its words joined by single
    /// spaces. Char offsets in [`LineRun::char_start`] index into this.
    pub text: String,
    /// Total chars in [`Self::text`].
    pub char_len: usize,
}

/// A fully laid-out chapter plus its page breaks.
#[derive(Clone, Debug)]
pub struct ChapterLayout {
    pub blocks: Vec<LaidBlock>,
    pub total_height: f32,
    /// Absolute y at which each page begins. `pages.len()` == page count.
    pub pages: Vec<f32>,
    pub key: LayoutKey,
}

impl ChapterLayout {
    pub fn page_count(&self) -> usize {
        self.pages.len().max(1)
    }
}

/// Cache key: layout is invalid if any of these change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutKey {
    pub chapter: usize,
    pub width_q: i32,
    pub height_q: i32,
    pub scale_q: i32,
    pub spacing_q: i32,
    /// Signature of the selected font (family + per-face size multiplier), so a
    /// font change relays out.
    pub font_sig: u64,
}

impl LayoutKey {
    pub fn new(
        chapter: usize,
        content_w: f32,
        content_h: f32,
        s: &Settings,
        font_sig: u64,
    ) -> Self {
        Self {
            chapter,
            width_q: content_w.round() as i32,
            height_q: content_h.round() as i32,
            scale_q: (s.font_scale * 100.0).round() as i32,
            spacing_q: (s.line_spacing * 100.0).round() as i32,
            font_sig,
        }
    }
}

/// Per-block style derived from kind + settings.
struct BlockStyle {
    kind: LayoutKind,
    size: f32,
    /// Base inline style applied to every run (quotes are italic; mono forces).
    force_italic: bool,
    force_bold: bool,
    indent: f32,
    space_before: f32,
    /// Fixed total height for blocks with no flowed text (separators).
    fixed_height: Option<f32>,
}

fn style_for(block: &Block, body: f32) -> BlockStyle {
    match block {
        Block::Heading { level, .. } => {
            let (mult, sb) = match level {
                1 => (1.62, 1.6),
                2 => (1.32, 1.3),
                _ => (1.14, 1.0),
            };
            BlockStyle {
                kind: LayoutKind::Heading,
                size: body * mult,
                force_italic: false,
                force_bold: true,
                indent: 0.0,
                space_before: body * sb,
                fixed_height: None,
            }
        }
        Block::Paragraph { .. } => BlockStyle {
            kind: LayoutKind::Paragraph,
            size: body,
            force_italic: false,
            force_bold: false,
            indent: 0.0,
            space_before: body * 0.75,
            fixed_height: None,
        },
        Block::Quote { .. } => BlockStyle {
            kind: LayoutKind::Quote,
            size: body * 1.02,
            force_italic: true,
            force_bold: false,
            indent: 30.0,
            space_before: body * 0.9,
            fixed_height: None,
        },
        Block::ListItem { .. } => BlockStyle {
            kind: LayoutKind::List,
            size: body,
            force_italic: false,
            force_bold: false,
            indent: 28.0,
            space_before: body * 0.4,
            fixed_height: None,
        },
        Block::Separator => BlockStyle {
            kind: LayoutKind::Separator,
            size: body,
            force_italic: false,
            force_bold: false,
            indent: 0.0,
            space_before: body * 1.2,
            fixed_height: Some(body * 1.6),
        },
    }
}

/// A word carrying its inline style.
struct Word {
    text: String,
    bold: bool,
    italic: bool,
    mono: bool,
    w: f32,
    /// Whether a space separates this word from the previous one. `false` lets
    /// punctuation and clitics hug the preceding word across a style boundary.
    space_before: bool,
    /// Char offset of this word's first char within the block's canonical text.
    char_start: usize,
    char_len: usize,
}

fn weight(bold: bool) -> TextWeight {
    if bold {
        TextWeight::Bold
    } else {
        TextWeight::Regular
    }
}

/// Lay out one chapter into positioned lines + page breaks. `base_family` is
/// the reader's chosen body face (serif / a named installed family / sans);
/// `font_mult` scales the nominal body size for that face; `font_sig` feeds the
/// layout cache key so a font change relays out.
// Layout entry point: chapter/text-renderer/box/typography params are the
// natural, minimal parameter set for the pagination engine.
#[allow(clippy::too_many_arguments)]
pub fn layout_chapter(
    tr: &mut TextRenderer,
    blocks: &[Block],
    content_w: f32,
    content_h: f32,
    settings: &Settings,
    chapter: usize,
    base_family: &FontChoice,
    font_mult: f32,
    font_sig: u64,
) -> ChapterLayout {
    let key = LayoutKey::new(chapter, content_w, content_h, settings, font_sig);
    let body = settings.body_size_with(font_mult);
    let base_mono = matches!(base_family, FontChoice::Monospace);
    let mut laid: Vec<LaidBlock> = Vec::with_capacity(blocks.len());
    let mut y = 0.0f32;

    for (index, block) in blocks.iter().enumerate() {
        let st = style_for(block, body);
        y += st.space_before;

        if st.kind == LayoutKind::Separator {
            let h = st.fixed_height.unwrap_or(st.size * 1.6);
            laid.push(LaidBlock {
                index,
                kind: LayoutKind::Separator,
                top: y,
                bottom: y + h,
                size: st.size,
                line_height: h,
                indent: 0.0,
                lines: vec![LaidLine { y, runs: vec![] }],
                bullet: None,
                text: String::new(),
                char_len: 0,
            });
            y += h;
            continue;
        }

        let line_height = st.size * 1.25 * settings.line_spacing;
        let text_width = (content_w - st.indent).max(40.0);
        let space_w = measure_space(tr, st.size, base_family);

        // Build the word list with measured widths, and the block's canonical
        // selectable text (words joined by single spaces). The first word of
        // each span inherits the span's `space_before`; later words are
        // whitespace-separated by construction.
        let mut words: Vec<Word> = Vec::new();
        let mut canon = String::new();
        let mut char_pos = 0usize;
        for span in block.spans() {
            let bold = span.bold || st.force_bold;
            let italic = span.italic || st.force_italic;
            let mono = span.code || base_mono;
            let word_family = if mono {
                &FontChoice::Monospace
            } else {
                base_family
            };
            for (wi, w) in span.text.split_whitespace().enumerate() {
                let (ww, _h) = tr.measure_font(w, st.size, None, weight(bold), italic, word_family);
                let space_before = if wi == 0 { span.space_before } else { true };
                // A separating space is one canonical char between words.
                if !canon.is_empty() && space_before {
                    canon.push(' ');
                    char_pos += 1;
                }
                let char_start = char_pos;
                let char_len = w.chars().count();
                canon.push_str(w);
                char_pos += char_len;
                words.push(Word {
                    text: w.to_string(),
                    bold,
                    italic,
                    mono,
                    w: ww,
                    space_before,
                    char_start,
                    char_len,
                });
            }
        }

        // Greedy word wrap. A word joins with a leading space only when
        // `space_before` says so — otherwise it abuts the previous run.
        let mut lines: Vec<LaidLine> = Vec::new();
        let mut cur: Vec<LineRun> = Vec::new();
        let mut cur_w = 0.0f32;
        let block_top = y;
        let mut line_idx = 0usize;
        for word in &words {
            let gap = if !cur.is_empty() && word.space_before {
                space_w
            } else {
                0.0
            };
            if !cur.is_empty() && cur_w + gap + word.w > text_width {
                lines.push(LaidLine {
                    y: block_top + line_idx as f32 * line_height,
                    runs: std::mem::take(&mut cur),
                });
                line_idx += 1;
                cur_w = 0.0;
            }
            let x = if cur.is_empty() { 0.0 } else { cur_w + gap };
            cur.push(LineRun {
                x,
                text: word.text.clone(),
                bold: word.bold,
                italic: word.italic,
                mono: word.mono,
                char_start: word.char_start,
                char_len: word.char_len,
            });
            cur_w = x + word.w;
        }
        if !cur.is_empty() || lines.is_empty() {
            lines.push(LaidLine {
                y: block_top + line_idx as f32 * line_height,
                runs: cur,
            });
            line_idx += 1;
        }

        let bullet = match block {
            Block::ListItem {
                ordered: true,
                index: n,
                ..
            } => Some(format!("{n}.")),
            Block::ListItem { ordered: false, .. } => Some("•".to_string()),
            _ => None,
        };

        let height = line_idx as f32 * line_height;
        laid.push(LaidBlock {
            index,
            kind: st.kind,
            top: block_top,
            bottom: block_top + height,
            size: st.size,
            line_height,
            indent: st.indent,
            lines,
            bullet,
            char_len: char_pos,
            text: canon,
        });
        y += height;
    }

    let total_height = y;
    let pages = paginate(&laid, content_h);

    ChapterLayout {
        blocks: laid,
        total_height,
        pages,
        key,
    }
}

/// Slice the flow into page-start offsets at line boundaries.
fn paginate(blocks: &[LaidBlock], page_h: f32) -> Vec<f32> {
    if blocks.is_empty() || page_h <= 1.0 {
        return vec![0.0];
    }
    // Flatten to row boxes (absolute y, height) in order.
    let mut rows: Vec<(f32, f32)> = Vec::new();
    for b in blocks {
        if b.kind == LayoutKind::Separator {
            rows.push((b.top, b.bottom - b.top));
        } else {
            for l in &b.lines {
                rows.push((l.y, b.line_height));
            }
        }
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut pages = vec![rows[0].0];
    let mut page_start = rows[0].0;
    for (y, h) in &rows {
        if y + h - page_start > page_h + 0.5 {
            page_start = *y;
            pages.push(page_start);
        }
    }
    pages
}

/// Width of a single space at `size` in `family`, derived by differencing.
fn measure_space(tr: &mut TextRenderer, size: f32, family: &FontChoice) -> f32 {
    let w2 = tr
        .measure_font("m m", size, None, TextWeight::Regular, false, family)
        .0;
    let w1 = tr
        .measure_font("mm", size, None, TextWeight::Regular, false, family)
        .0;
    (w2 - w1).max(size * 0.22)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, Span};

    fn para(text: &str) -> Block {
        Block::Paragraph {
            spans: vec![Span::plain(text)],
        }
    }

    #[test]
    fn layout_produces_pages_and_lines() {
        let mut tr = TextRenderer::new();
        let blocks = vec![
            Block::Heading {
                level: 1,
                spans: vec![Span::plain("A Chapter")],
            },
            para(
                "This is a reasonably long paragraph that should wrap across \
                  several lines when laid out into a narrow content column so \
                  that pagination has something real to slice into pages here.",
            ),
            Block::Separator,
            para(
                "Another paragraph after the rule, also long enough to wrap a \
                  couple of times within the narrow measure we hand the engine.",
            ),
        ];
        let layout = layout_chapter(
            &mut tr,
            &blocks,
            360.0,
            220.0,
            &Settings::default(),
            0,
            &FontChoice::SansSerif,
            1.0,
            0,
        );
        assert!(layout.total_height > 0.0);
        assert!(layout.page_count() >= 1);
        // The long paragraph wrapped into multiple lines.
        let para_block = &layout.blocks[1];
        assert!(para_block.lines.len() >= 2, "paragraph should wrap");
        // Lines are monotonically increasing in y.
        let ys: Vec<f32> = para_block.lines.iter().map(|l| l.y).collect();
        assert!(ys.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn narrow_tall_content_paginates_into_several_pages() {
        let mut tr = TextRenderer::new();
        let big = "word ".repeat(400);
        let blocks = vec![para(&big)];
        let layout = layout_chapter(
            &mut tr,
            &blocks,
            300.0,
            160.0,
            &Settings::default(),
            0,
            &FontChoice::SansSerif,
            1.0,
            0,
        );
        assert!(layout.page_count() >= 3, "lots of text -> several pages");
        // Page starts strictly increase.
        assert!(layout.pages.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn layout_key_changes_with_settings_and_font() {
        let a = LayoutKey::new(0, 300.0, 200.0, &Settings::default(), 0);
        let s = Settings {
            font_scale: 1.4,
            ..Default::default()
        };
        let b = LayoutKey::new(0, 300.0, 200.0, &s, 0);
        assert_ne!(a, b, "font scale change relays out");
        // A different font signature (family/size-mult) also relays out.
        let c = LayoutKey::new(0, 300.0, 200.0, &Settings::default(), 99);
        assert_ne!(a, c, "font change relays out");
    }

    #[test]
    fn selection_char_offsets_are_assigned_across_words() {
        let mut tr = TextRenderer::new();
        let blocks = vec![para("alpha beta gamma")];
        let layout = layout_chapter(
            &mut tr,
            &blocks,
            600.0,
            400.0,
            &Settings::default(),
            0,
            &FontChoice::SansSerif,
            1.0,
            0,
        );
        let b = &layout.blocks[0];
        assert_eq!(
            b.text, "alpha beta gamma",
            "canonical text joins words with spaces"
        );
        // Each run carries a char range that indexes into the canonical text.
        let runs: Vec<&LineRun> = b.lines.iter().flat_map(|l| l.runs.iter()).collect();
        assert_eq!(runs.len(), 3);
        for r in &runs {
            let slice: String = b.text.chars().skip(r.char_start).take(r.char_len).collect();
            assert_eq!(slice, r.text, "run char range slices back to its text");
        }
    }
}
